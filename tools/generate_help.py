#!/usr/bin/env python3
"""Generate the offline Rust-friendly LinuxCNC G-code help bundle.

Source data is the pinned generated module shipped with Probe Basic:
    ../../baiermill_pb/user_tabs/gcode_help/gcode_docs.py
and its images. The generated JSON is embedded by linc-core; images are copied
to assets/help/images and embedded by linc-ui.

The output keeps the inline markup the source-highlighted HTML carries: bold,
italic, inline code and internal "#anchor" links become spans, the four
admonition tables become typed admonition blocks, and a table that marks a
header row says so. Everything the renderer cannot express stays in the plain
"text" field so search and the fallback still work.

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

# The cell background of an admonition table identifies its type; the keys are
# Probe Basic's _ADMON_REMAP source colours (gcode_help.py).
ADMONITION_KINDS = {
    "#fdecea": "warning",
    "#fff9c4": "note",
    "#e8f5e9": "tip",
    "#fff3e0": "important",
    "#ffe0b2": "caution",
}

SPAN_FLAGS = ("bold", "italic", "code")


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


def collapse(text: str) -> str:
    """Inline spans reflow: the source's newlines are line breaks, not text."""
    return re.sub(r"\s+", " ", text.replace("\xa0", " "))


def style_flags(tag: Tag) -> tuple[bool, bool, bool]:
    style = (tag.get("style") or "").replace(" ", "").lower()
    bold = tag.name.lower() in {"b", "strong"} or "font-weight:bold" in style
    italic = tag.name.lower() in {"i", "em"} or "font-style:italic" in style
    code = tag.name.lower() in {"code", "tt", "kbd"} or "monospace" in style
    return bold, italic, code


def anchor_of(tag: Tag) -> str | None:
    if tag.name.lower() != "a":
        return None
    href = tag.get("href") or ""
    if href.startswith("#") and len(href) > 1:
        return unquote(href[1:])
    return None


def inline_spans(node: Tag) -> list[dict]:
    """Flatten one block's inline markup into styled spans."""
    spans: list[dict] = []

    def add(text: str, bold: bool, italic: bool, code: bool, link: str | None) -> None:
        text = collapse(text)
        if not text:
            return
        if spans and spans[-1].get("link") == link and all(
            spans[-1].get(flag, False) is value for flag, value in zip(SPAN_FLAGS, (bold, italic, code))
        ):
            spans[-1]["text"] += text
            return
        span = {"text": text}
        for flag, value in zip(SPAN_FLAGS, (bold, italic, code)):
            if value:
                span[flag] = True
        if link:
            span["link"] = link
        spans.append(span)

    def walk(current: Tag, bold: bool, italic: bool, code: bool, link: str | None) -> None:
        for child in current.children:
            if isinstance(child, NavigableString):
                add(str(child), bold, italic, code, link)
            elif isinstance(child, Tag):
                if child.name.lower() in {"br", "hr"}:
                    add(" ", bold, italic, code, link)
                    continue
                child_bold, child_italic, child_code = style_flags(child)
                walk(
                    child,
                    bold or child_bold,
                    italic or child_italic,
                    code or child_code,
                    anchor_of(child) or link,
                )

    walk(node, False, False, False, None)
    if spans:
        spans[0]["text"] = spans[0]["text"].lstrip()
        spans[-1]["text"] = spans[-1]["text"].rstrip()
        spans = [span for span in spans if span["text"]]
    return spans


def span_text(spans: list[dict]) -> str:
    return "".join(span["text"] for span in spans)


def image_blocks(node: Tag) -> list[dict]:
    blocks = []
    for image in node.find_all("img"):
        src = image.get("src", "")
        name = Path(unquote(src)).name
        alt = clean(image.get("alt", ""))
        blocks.append({"kind": "image", "image": name, "text": alt})
    return blocks


def table_has_header(table: Tag) -> bool:
    if table.find("thead"):
        return True
    first_row = table.find("tr")
    return bool(first_row and first_row.find("th"))


def table_block(table: Tag) -> dict:
    rows = []
    for tr in table.find_all("tr"):
        cells = [clean(td.get_text(" ", strip=True)) for td in tr.find_all(["th", "td"])]
        if cells:
            rows.append(cells)
    block = {"kind": "table", "rows": rows}
    if table_has_header(table):
        block["header"] = True
    return block


def admonition_block(cell: Tag, kind: str) -> dict:
    """One admonition table: its type, its prose, and the prose's spans."""
    spans = inline_spans(cell)
    if spans:
        label = spans[0]["text"].strip().lower()
        if label in {"warning", "note", "tip", "important", "caution"}:
            spans = spans[1:]
            if spans:
                spans[0]["text"] = spans[0]["text"].lstrip(" —-–\t")
                spans = [span for span in spans if span["text"]]
    block = {"kind": "admonition", "admonition": kind, "text": span_text(spans)}
    if spans:
        block["spans"] = spans
    return block


def single_cell_payload(table: Tag) -> list[dict] | None:
    """The docs wrap distinct things in one-cell tables; unwrap the containers.

    A one-cell table is either an admonition (its cell carries the type's
    background), a code block (its cell holds only <pre>), or a plain single
    cell, which stays a table.
    """
    rows = table.find_all("tr")
    cells = table.find_all("td")
    if len(rows) != 1 or len(cells) != 1 or table.find("table"):
        return None
    cell = cells[0]
    kind = ADMONITION_KINDS.get((cell.get("bgcolor") or "").strip().lower())
    if kind:
        return [admonition_block(cell, kind)]
    pres = cell.find_all("pre")
    if pres:
        blocks = []
        for pre in pres:
            text = pre.get_text().strip("\n\r")
            if text.strip():
                blocks.append({"kind": "code", "text": text})
        if len(cell.get_text().strip()) == sum(len(pre.get_text().strip("\n\r")) for pre in pres):
            return blocks
    return None


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
        payload = single_cell_payload(node)
        if payload is not None:
            return payload
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
            blocks.append(table)
        return blocks
    if name in {"ul", "ol"}:
        items = []
        for li in node.find_all("li", recursive=False):
            items.extend(image_blocks(li))
            spans = inline_spans(li)
            if spans:
                item = {"kind": "list_item", "text": span_text(spans), "spans": spans}
                items.append(item)
        if items:
            blocks.append({"kind": "list", "items": items})
        return blocks
    if name == "img":
        return image_blocks(node)
    if name in {"p", "div", "section", "blockquote", "dd", "dt"}:
        images = image_blocks(node)
        spans = inline_spans(node)
        if images:
            blocks.extend(images)
            if spans:
                blocks.append({"kind": "para", "text": span_text(spans), "spans": spans})
            return blocks
        if spans:
            blocks.append({"kind": "para", "text": span_text(spans), "spans": spans})
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
