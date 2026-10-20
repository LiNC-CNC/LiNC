#!/usr/bin/env python3
"""Generate the offline Rust-friendly LinuxCNC G-code help bundle.

Source data is the pinned generated module shipped with Probe Basic:
    ../../baiermill_pb/user_tabs/gcode_help/gcode_docs.py
and its images. The generated JSON is embedded by linc-core; images are copied
to assets/help/images and embedded by linc-ui.

Usage (from LiNC/):
    python3 tools/generate_help.py
"""
from __future__ import annotations

import importlib.util
import json
import re
import shutil
import sys
from pathlib import Path
from urllib.parse import unquote

from bs4 import BeautifulSoup, NavigableString, Tag

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT.parent / "baiermill_pb" / "user_tabs" / "gcode_help"
OUT = ROOT / "crates" / "linc-core" / "assets" / "gcode_help.json"
IMAGES_OUT = ROOT / "assets" / "help" / "images"


def load_docs():
    spec = importlib.util.spec_from_file_location("gcode_docs", SOURCE / "gcode_docs.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.INDEX, module.SECTIONS


def clean(text: str) -> str:
    text = text.replace("\xa0", " ")
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n\s*\n+", "\n\n", text)
    return text.strip()


def image_blocks(node: Tag) -> list[dict]:
    blocks = []
    for image in node.find_all("img"):
        src = image.get("src", "")
        name = Path(unquote(src)).name
        alt = clean(image.get("alt", ""))
        blocks.append({"kind": "image", "image": name, "text": alt})
    return blocks


def table_block(table: Tag) -> dict:
    rows = []
    for tr in table.find_all("tr"):
        cells = [clean(td.get_text(" ", strip=True)) for td in tr.find_all(["th", "td"])]
        if cells:
            rows.append(cells)
    return {"kind": "table", "rows": rows}


def node_blocks(node: Tag) -> list[dict]:
    blocks: list[dict] = []
    if isinstance(node, NavigableString):
        return blocks
    name = node.name.lower()
    if name in {"h1", "h2", "h3", "h4", "h5", "h6"}:
        text = clean(node.get_text(" ", strip=True))
        if text:
            blocks.append({"kind": "heading", "text": text, "level": int(name[1])})
        return blocks
    if name == "pre":
        text = node.get_text().strip("\n\r")
        if text.strip():
            blocks.append({"kind": "code", "text": text})
        return blocks
    if name == "table":
        blocks = []
        images = image_blocks(node)
        if images:
            blocks.extend(images)
        for pre in node.find_all("pre"):
            text = pre.get_text().strip("\n\r")
            if text.strip():
                blocks.append({"kind": "code", "text": text})
        table = table_block(node)
        # A table whose only content was a figure or code block is layout, not
        # a parameter table.
        meaningful_rows = [
            row
            for row in table["rows"]
            if any(cell and cell not in {"", " "} for cell in row)
        ]
        if meaningful_rows and not images:
            if not (len(meaningful_rows) == 1 and not node.find("pre")):
                blocks.append({"kind": "table", "rows": meaningful_rows})
            elif len(meaningful_rows) == 1:
                blocks.append({"kind": "table", "rows": meaningful_rows})
        return blocks
    if name in {"ul", "ol"}:
        items = []
        for li in node.find_all("li", recursive=False):
            items.extend(image_blocks(li))
            text = clean(li.get_text(" ", strip=True))
            if text:
                items.append({"kind": "list_item", "text": text})
        if items:
            blocks.append({"kind": "list", "items": items})
        return blocks
    if name == "img":
        return image_blocks(node)
    if name in {"p", "div", "section", "blockquote", "dd", "dt"}:
        images = image_blocks(node)
        text = clean(node.get_text(" ", strip=True))
        if images:
            blocks.extend(images)
            if text:
                blocks.append({"kind": "para", "text": text})
            return blocks
        if text:
            if name == "blockquote":
                blocks.append({"kind": "note", "text": text})
            else:
                blocks.append({"kind": "para", "text": text})
        # Some wrappers contain lists/tables not covered by get_text.
        if name in {"div", "section"} and node.find(["ul", "ol", "table", "pre"]):
            for child in node.children:
                if isinstance(child, Tag) and child.name in {"ul", "ol", "table", "pre"}:
                    blocks.extend(node_blocks(child))
        return blocks
    # Generic container: recurse into children.
    for child in node.children:
        if isinstance(child, Tag):
            blocks.extend(node_blocks(child))
    return blocks


def section_blocks(html: str) -> tuple[list[dict], list[str]]:
    soup = BeautifulSoup(html, "html.parser")
    blocks: list[dict] = []
    for child in soup.children:
        if isinstance(child, Tag):
            blocks.extend(node_blocks(child))
    related = []
    for a in soup.find_all("a"):
        href = a.get("href", "")
        if href.startswith("#") and len(href) > 1:
            anchor = unquote(href[1:])
            if anchor and anchor not in related:
                related.append(anchor)
    return blocks, related


def main() -> int:
    index, sections = load_docs()
    groups = []
    for group in index:
        groups.append(
            {
                "name": group.get("name", ""),
                "note": group.get("note", ""),
                "rows": [
                    {
                        "anchor": row.get("anchor", ""),
                        "code": row.get("code", ""),
                        "params": row.get("params", ""),
                        "desc": row.get("desc", ""),
                        "meaning": row.get("meaning", ""),
                    }
                    for row in group.get("rows", [])
                ],
            }
        )
    out_sections = {}
    anchors = set()
    image_names = set()
    for key, value in sections.items():
        blocks, related = section_blocks(value.get("html", ""))
        for block in blocks:
            if block.get("kind") == "image":
                image_names.add(block["image"])
            if block.get("kind") == "list":
                for item in block.get("items", []):
                    if item.get("kind") == "image":
                        image_names.add(item["image"])
        out_sections[key] = {
            "title": value.get("title", key),
            "blocks": blocks,
            "related": related,
        }
        anchors.add(key)
    for section in out_sections.values():
        section["related"] = [
            anchor for anchor in section["related"] if anchor in anchors
        ]

    payload = {
        "source": "https://linuxcnc.org/docs/html/gcode",
        "bundle": "linuxcnc-gcode",
        "version": "pinned Probe Basic gcode_docs.py bundle",
        "attribution": "LinuxCNC documentation contributors; generated by build_gcode_docs.py",
        "counts": {
            "sections": len(out_sections),
            "groups": len(groups),
            "rows": sum(len(group["rows"]) for group in groups),
            "figures": len(image_names),
        },
        "groups": groups,
        "sections": out_sections,
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(payload, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")

    IMAGES_OUT.mkdir(parents=True, exist_ok=True)
    copied = 0
    for name in sorted(image_names):
        source = SOURCE / "images" / name
        if not source.is_file():
            print(f"missing image: {source}", file=sys.stderr)
            continue
        shutil.copy2(source, IMAGES_OUT / name)
        copied += 1
    print(
        f"wrote {OUT.relative_to(ROOT)} "
        f"({OUT.stat().st_size} bytes): {len(out_sections)} sections, "
        f"{len(groups)} groups, {sum(len(g['rows']) for g in groups)} rows, "
        f"{copied} figures"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
