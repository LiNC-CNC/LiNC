#!/usr/bin/env python3
"""Convert a PLCYard `.plcy` project to PLCopen TC6 XML.

This is the first half of the corpus chain: `sources/*.plcy` -> `*.xml` ->
`projects/*.json` (see `tools/plcy2json`). It is written against the corpus and
emits only what LiNC's importer accepts; a construct it does not know is a hard
error rather than a guess, so a conversion can never silently differ.

Usage:
    plcy2plcopen.py <sources-dir> <out-dir>
    plcy2plcopen.py <one.plcy> <out.xml>

Licence: GPL-3.0-or-later, with the rest of this directory.
"""

import json
import os
import sys

# Instructions whose block keeps its name, and their preset pin.
BLOCKS = {
    "TON": "PT",
    "TOF": "PT",
    "RTO": "PT",
    "CTU": "PV",
    "CTD": "PV",
}
COMPARES = {"EQU": "EQ", "NEQ": "NE", "LES": "LT", "LEQ": "LE", "GRT": "GT", "GEQ": "GE"}
# Series coils: mnemonic -> storage attribute.
COILS = {"OTE": "none", "OTL": "set", "OTU": "reset"}
# Analogue instructions: their pins, in order. A `?` operand leaves the pin out.
ANALOG = {
    "SCP": (False, ["IN", "MIN_IN", "MAX_IN", "MIN_OUT", "MAX_OUT", "OUT"]),
    "PID": (True, ["IN", "OUT", "SP", "TIEBACK", "MASTER"]),
}
# A PID instance's setup, emitted as pins in this order.
PID_MEMBERS = [
    "KP", "KI", "KD", "MINS", "MAXS", "MINI", "MAXI",
    "MINO", "MAXO", "MINCV", "MAXCV", "UPD", "BIAS",
]
# The member of a timer or counter a `.plcy` name refers to, and its XML name.
MEMBERS = {
    "DN": "Q",
    "ACC": "ET",
    "ET": "ET",
    "TT": "TT",
    "Q": "Q",
    "R": "R",
}
SCALAR_TYPES = {"BOOL", "INT", "DINT", "REAL", "TIME", "SINT", "LINT", "WORD"}
INSTANCE_TYPES = {"TIMER": "TON", "COUNTER": "CTU"}


class Refuse(Exception):
    pass


# -- the source model ----------------------------------------------------


class Var:
    """A declared tag: name, type, and its initial value if it has one."""

    def __init__(self, name, spec):
        self.name = name
        self.spec = spec
        self.raw = str(spec.get("type", "BOOL"))
        self.count = 1
        self.is_array = self.raw.endswith("]") and "[" in self.raw
        base = self.raw
        if self.is_array:
            base, _, count = self.raw[:-1].partition("[")
            self.count = int(count)
        self.base = base
        self.value = spec.get("value")

    @property
    def family(self):
        """`TIMER`/`COUNTER` for an instance tag, else None."""
        return self.base if self.base in INSTANCE_TYPES else None


class Source:
    def __init__(self, path):
        try:
            with open(path) as handle:
                self.doc = json.load(handle)
        except (OSError, json.JSONDecodeError) as error:
            raise Refuse(f"{path}: {error}") from error
        self.stem = os.path.splitext(os.path.basename(path))[0]
        self.tags = {name: Var(name, spec) for name, spec in (self.doc.get("tags") or {}).items()}
        self.structs = self.doc.get("types") or {}
        # A tag of a function-block (or controller) type is an instance, not a
        # declared variable: its state lives in the block.
        self.instance_types = {
            str(program.get("title", "")).upper() for program in self.doc.get("programs") or []
        } | {"PID"}


# -- tokenising and the rung tree ----------------------------------------


def tokens(rung_text):
    return rung_text.split()


def operand(text):
    return text


class Item:
    pass


class Contact(Item):
    def __init__(self, var, negated):
        self.var = var
        self.negated = negated


class Coil(Item):
    def __init__(self, kind, var):
        self.kind = kind
        self.var = var


class Instr(Item):
    def __init__(self, mnemonic, args):
        self.mnemonic = mnemonic
        self.args = args


class Parallel(Item):
    def __init__(self, branches):
        self.branches = branches


def parse(text):
    """A rung's tokens as a list of series items, `Parallel` for branch groups."""
    series: list[Item] = []
    stack: list[tuple[int, list[list[Item]]]] = []  # (level, branches)
    for token in tokens(text):
        head, _, rest = token.partition(",")
        args = rest.split(",") if rest else []
        if head == "BST":
            stack.append((int(args[0]), [series]))
            series = []
        elif head == "NXB":
            stack[-1][1].append(series)
            series = []
        elif head == "BND":
            _, branches = stack.pop()
            branches.append(series)
            series = [Parallel(branches)]
        elif head in ("XIC", "XIO"):
            series.append(Contact(args[0], head == "XIO"))
        elif head in COILS:
            series.append(Coil(COILS[head], args[0]))
        elif head == "ONS":
            series.append(Instr("ONS", args))
        else:
            series.append(Instr(head, args))
    if stack:
        raise Refuse(f"unbalanced branch group in {text!r}")
    return series


def storer(item):
    """True when this item is a rung end that stores its power somewhere."""
    if isinstance(item, Coil):
        return True
    return isinstance(item, Instr) and item.mnemonic in (
        "ONS", "RES", "MOV", "ADD", "CALL", "JSR"
    )


def coils_of(items):
    """Every storing rung end in a rung tree, in source order."""
    found = []
    for item in items:
        if storer(item):
            found.append(item)
        elif isinstance(item, Parallel):
            for branch in item.branches:
                found.extend(coils_of(branch))
    return found


def is_writer(item):
    """True when the item stores its power somewhere: a coil, a call, a compute."""
    if isinstance(item, Coil):
        return True
    if isinstance(item, Instr):
        return item.mnemonic in ("ONS", "RES", "MOV", "ADD", "CALL", "JSR", "SCP", "PID")
    return False


def prune(items, target):
    """The series that powers `target`, with sibling branches dropped."""
    kept = []
    for item in items:
        if item is target:
            kept.append(item)
            return kept
        if isinstance(item, Parallel):
            for branch in item.branches:
                if contains(branch, target):
                    kept.append(Parallel([prune(branch, target)]))
                    return kept
            # The parallel is on the way to the coil, not beside it: every
            # branch feeds the coil, so all of them stay.
            kept.append(item)
            continue
        if isinstance(item, Instr) and item.mnemonic in ("MOV", "ADD", "CALL", "JSR", "RES",
                                                          "SCP", "PID"):
            # A compute or call consumes the rung: nothing after it is in series.
            continue
        if isinstance(item, Coil):
            continue
        kept.append(item)
    return kept


def contains(items, target):
    for item in items:
        if item is target:
            return True
        if isinstance(item, Parallel) and any(contains(branch, target) for branch in item.branches):
            return True
    return False


# -- emitting ------------------------------------------------------------


class Pou:
    """One POU: its name, kind, interface and body lines."""

    def __init__(self, name, kind, source):
        self.name = name
        self.kind = kind
        self.source = source
        self.inputs = {}
        self.outputs = {}
        self.locals = {}
        self.lines = []
        self.next_id = 10
        self.instances = {}  # (family, key) -> name
        self.counts = {}
        # A dry pass numbers the instances in the order the blocks appear, which
        # is the order the importer numbers them. Contacts are named from that map
        # and never allocate a name themselves, or a contact above the block it
        # reads would shift every instance below it.
        self.dry = False

    # -- operands ---------------------------------------------------------

    def instance_name(self, family, key, peek=False):
        """The instance a timer/counter tag element is named `TON0`/`CTU1`."""
        if (family, key) not in self.instances:
            if peek:
                return f"{family}{self.counts.get(family, 0)}"
            index = self.counts.get(family, 0)
            self.counts[family] = index + 1
            self.instances[(family, key)] = f"{family}{index}"
        return self.instances[(family, key)]

    def tag_of(self, var):
        """`Tag[2].DN` -> (Var, element key, member) or None for a plain tag."""
        name, _, member = var.partition(".")
        base, element = name, None
        if "[" in name:
            head, _, tail = name[:-1].partition("[")
            base = head
            element = int(tail)
        tag = self.source.tags.get(base) or self.source.tags.get(base.lower())
        return tag, element, member

    def declared(self, var):
        """The name the XML uses for a `.plcy` operand."""
        if var.startswith("#"):
            name = var[1:]
            tag = self.source.tags.get(name)
            if tag and tag.family:
                return self.instance_name(INSTANCE_TYPES[tag.family], name, peek=self.dry)
            return name
        tag, element, member = self.tag_of(var)
        if tag is not None and tag.family:
            key = element if element is not None else 0
            name = self.instance_name(
                INSTANCE_TYPES[tag.family], (tag.name, key), peek=self.dry
            )
            if member:
                if member not in MEMBERS:
                    raise Refuse(f"{var}: `{member}` is not a timer or counter member")
                return f"{name}.{MEMBERS[member]}"
            return name
        if member:
            # A struct field or a POU variable keeps its dotted name.
            return var
        return var

    def declare(self, var):
        """Record a declared tag as an interface variable."""
        if var in self.inputs or var in self.outputs or var in self.locals:
            return
        tag = self.source.tags.get(var)
        if tag is not None:
            self.locals[var] = tag
        # Percent addresses are collected with their direction from use.

    # -- body -------------------------------------------------------------

    def emit(self, rung):
        tree = parse(rung["text"])
        ends = coils_of(tree)
        if not ends:
            # A rung that ends in a block and stores nothing: the block is the
            # rung (a timer on a condition). The importer treats it as one.
            refs = ["0"]
            for item in tree:
                refs = self.walk(item, refs)
            return
        for coil in ends:
            path = prune(tree, coil)
            refs = ["0"]
            for item in path[:-1]:
                refs = self.walk(item, refs)
            self.store_coil(coil, refs)

    def walk(self, item, refs):
        if isinstance(item, Contact):
            local = self.new_id()
            negated = ' negated="true"' if item.negated else ""
            var = self.declared(item.var)
            self.connections(var, refs, want="in")
            self.lines.append(
                f'<contact localId="{local}"{negated}>{self.points_in(refs)}'
                f"<variable>{var}</variable></contact>"
            )
            return [local]
        if isinstance(item, Parallel):
            tails = []
            for branch in item.branches:
                branch_refs = refs
                for inner in branch:
                    branch_refs = self.walk(inner, branch_refs)
                tails.extend(branch_refs)
            return tails
        if isinstance(item, Instr):
            return self.instruction(item, refs)
        if isinstance(item, Coil):
            return self.store_coil(item, refs)
        raise Refuse(f"unknown item {item!r}")

    def instruction(self, item, refs):
        mnemonic = item.mnemonic
        args = item.args
        if mnemonic in COMPARES:
            local = self.new_id()
            pins = "".join(
                f'<variable formalParameter="IN"><expression>{self.declared(arg)}</expression>'
                f"</variable>"
                for arg in args
            )
            self.lines.append(
                f'<block localId="{local}" typeName="{COMPARES[mnemonic]}">'
                f"{self.points_in(refs)}<inputVariables>{pins}</inputVariables></block>"
            )
            return [local]
        if mnemonic in BLOCKS:
            local = self.new_id()
            tag, element, member = self.tag_of(args[0])
            if tag is None and not args[0].startswith("#"):
                raise Refuse(f"{mnemonic}: `{args[0]}` is not a declared instance")
            self.instance_name(mnemonic, self.instance_key(args[0]))
            preset = args[1] if len(args) > 1 else ""
            pins = ""
            if preset:
                # A timer's preset is a time and may be written bare; a counter's
                # is a count.
                text = self.preset(preset) if mnemonic in ("TON", "TOF", "RTO") else preset
                pins = (
                    f'<variable formalParameter="{BLOCKS[mnemonic]}">'
                    f"<expression>{text}</expression></variable>"
                )
            self.lines.append(
                f'<block localId="{local}" typeName="{mnemonic}">{self.points_in(refs)}'
                f"<inputVariables>{pins}</inputVariables></block>"
            )
            return [local]
        if mnemonic == "ONS":
            # A one-shot is a rising-edge block plus the memory coil it writes.
            local = self.new_id()
            self.lines.append(
                f'<block localId="{local}" typeName="R_TRIG">{self.points_in(refs)}</block>'
            )
            self.instance_name("RT", self.instance_key("ONS@" + args[0]))
            return [local]
        if mnemonic in ANALOG:
            return self.analog(mnemonic, args, refs)
        if mnemonic in ("MOV", "ADD", "CALL", "JSR"):
            return self.store_coil(item, refs)
        if mnemonic == "RES":
            local = self.new_id()
            var = self.declared(args[0])
            self.connections(var, refs, want="out")
            self.lines.append(
                f'<coil localId="{local}" storage="reset">{self.points_in(refs)}'
                f"<variable>{var}.R</variable></coil>"
            )
            return [local]
        raise Refuse(f"instruction {mnemonic} has no rung mapping")

    def instance_key(self, name):
        tag, element, _ = self.tag_of(name)
        if tag is not None and tag.family:
            return (tag.name, element if element is not None else 0)
        return name

    def preset(self, text):
        """A preset as the importer reads it: a bare number is milliseconds."""
        if text.lstrip("-").isdigit():
            return f"T#{text}ms"
        return text

    def analog(self, mnemonic, args, refs):
        has_instance, pins = ANALOG[mnemonic]
        if not args:
            raise Refuse(f"{mnemonic}: no instance")
        name = ""
        values = args
        if has_instance:
            name = args[0]
            values = args[1:]
        local = self.new_id()
        pins_out = []
        for pin, value in zip(pins, values):
            if value in ("?", ""):
                continue
            pins_out.append(
                f'<variable formalParameter="{pin}"><expression>{self.declared(value)}'
                f"</expression></variable>"
            )
        if has_instance:
            tag = self.source.tags.get(name)
            if tag is None:
                raise Refuse(f"{mnemonic}: `{name}` is not a declared controller")
            setup = tag.value or {}
            for member in PID_MEMBERS:
                if member in setup:
                    pins_out.append(
                        f'<variable formalParameter="{member}"><expression>{setup[member]}'
                        f"</expression></variable>"
                    )
        instance = f"<variable>{name.upper()}</variable>" if has_instance else ""
        self.lines.append(
            f'<block localId="{local}" typeName="{mnemonic}">{instance}{self.points_in(refs)}'
            f"<inputVariables>{''.join(pins_out)}</inputVariables></block>"
        )
        return [local]

    def store_coil(self, item, refs):
        """The coil, compute or call an item stores power in."""
        local = self.new_id()
        if isinstance(item, Coil):
            var = self.declared(item.var)
            self.connections(var, refs, want="out")
            storage = f' storage="{item.kind}"' if item.kind != "none" else ""
            self.lines.append(
                f'<coil localId="{local}"{storage}>{self.points_in(refs)}'
                f"<variable>{var}</variable></coil>"
            )
            return [local]
        mnemonic, args = item.mnemonic, item.args
        if mnemonic == "RES":
            var = self.declared(args[0])
            self.connections(var, refs, want="out")
            self.lines.append(
                f'<coil localId="{local}" storage="reset">{self.points_in(refs)}'
                f"<variable>{var}.R</variable></coil>"
            )
            return [local]
        if mnemonic == "ONS":
            var = self.declared(args[0])
            self.connections(var, refs, want="out")
            self.lines.append(
                f'<coil localId="{local}">{self.points_in(refs)}'
                f"<variable>{var}</variable></coil>"
            )
            return [local]
        if mnemonic in ("MOV", "ADD"):
            if mnemonic == "MOV":
                expression = f"{self.declared(args[1])} = {self.declared(args[0])}"
            else:
                expression = (
                    f"{self.declared(args[2])} = {self.declared(args[0])} + "
                    f"{self.declared(args[1])}"
                )
            self.connections(self.declared(args[-1]), refs, want="out")
            self.lines.append(
                f'<coil localId="{local}" storage="compute">{self.points_in(refs)}'
                f"<expression>{expression}</expression></coil>"
            )
            return [local]
        if mnemonic in ("JSR", "CALL"):
            name = args[0].upper()
            pins_in = []
            pins_out = []
            for argument in args[1:]:
                target, _, value = argument.partition(":=")
                destination, arrow, source = argument.partition("=>")
                if arrow:
                    pins_out.append(
                        f'<variable formalParameter="{destination.upper()}">'
                        f"<expression>{self.declared(source)}</expression></variable>"
                    )
                elif _:
                    pins_in.append(
                        f'<variable formalParameter="{target.upper()}">'
                        f"<expression>{self.declared(value)}</expression></variable>"
                    )
                else:
                    raise Refuse(f"{mnemonic}: argument `{argument}` has no := or =>")
            body = ""
            if pins_in:
                body += f"<inputVariables>{''.join(pins_in)}</inputVariables>"
            if pins_out:
                body += f"<outputVariables>{''.join(pins_out)}</outputVariables>"
            self.lines.append(
                f'<coil localId="{local}" storage="call">{self.points_in(refs)}'
                f"<variable>{name}</variable>{body}</coil>"
            )
            return [local]
        raise Refuse(f"{mnemonic} has no coil mapping")

    # -- interface --------------------------------------------------------

    def connections(self, var, refs, want):
        """Record the direction an operand was used in, and its address."""
        if var.startswith("%I") or var.startswith("%Q"):
            table = self.inputs if var.startswith("%I") else self.outputs
            table[var] = "INT" if var[2:3] == "W" else "BOOL"
        elif var.startswith("%"):
            (self.outputs if want == "out" else self.inputs)[var] = "WORD"
        else:
            base = var.split(".")[0].split("[")[0]
            if base.startswith("#"):
                return
            tag = self.source.tags.get(base) or self.source.tags.get(base.lower())
            if tag is not None and not tag.family and tag.base.upper() not in \
                    self.source.instance_types:
                self.locals[base] = tag

    def new_id(self):
        local = self.next_id
        self.next_id += 1
        return local

    def points_in(self, refs):
        return "<connectionPointIn>" + "".join(
            f'<connection refLocalId="{ref}"/>' for ref in refs
        ) + "</connectionPointIn>"

    def interface_xml(self):
        def variables(table, kind):
            if not table:
                return ""
            body = "".join(type_xml(name, value, self.source) for name, value in table.items())
            return f"<{kind}>{body}</{kind}>"

        inputs = {name: value for name, value in sorted(self.inputs.items())}
        outputs = {name: value for name, value in sorted(self.outputs.items())}
        locals_ = {name: value for name, value in sorted(self.locals.items())}
        body = variables(inputs, "inputVars") + variables(outputs, "outputVars") + \
            variables(locals_, "localVars")
        return f"<interface>{body}</interface>" if body else "<interface/>"


def type_xml(name, value, source):
    """One declared variable: its type and, when it has one, its initial value."""
    if isinstance(value, Var):
        tag = value
        initial = tag.value
        ty = tag.base
        count = tag.count if tag.is_array else 1
    else:
        tag = None
        initial = None
        ty = value
        count = 1
    if tag is not None:
        name = tag.name
    if tag is not None and tag.is_array:
        kind = type_body(ty, source)
        body = (
            f"<type><array><baseElementType>{kind}</baseElementType>"
            f'<dimension lower="0" upper="{count - 1}"/></array></type>'
        )
        if initial:
            body += initial_xml(tag, source)
        return f'<variable name="{name}">{body}</variable>'
    return f'<variable name="{name}"><type>{type_body(ty, source)}</type></variable>'


def type_body(ty, source):
    if ty in SCALAR_TYPES:
        return f"<{ty}/>"
    if ty in source.structs:
        return f'<derived name="{ty}"/>'
    raise Refuse(f"type `{ty}` has no PLCopen mapping")


def initial_xml(tag, source):
    """An initial value: a scalar, an array, or a struct array flattened."""
    value = tag.value
    if not isinstance(value, list):
        return f"<initialValue><simpleValue>{value}</simpleValue></initialValue>"
    values = []
    for element in value:
        if isinstance(element, dict):
            fields = list(source.structs.get(tag.base, {}).keys())
            values.extend(element.get(field, 0) for field in fields)
        else:
            values.append(element)
    body = "".join(f"<value>{item}</value>" for item in values)
    return f"<initialValue><arrayValue>{body}</arrayValue></initialValue>"


# -- the document --------------------------------------------------------


def convert(path):
    source = Source(path)
    programs = source.doc.get("programs") or []
    if not programs:
        raise Refuse(f"{path}: no programs")
    pous = []
    for index, program in enumerate(programs):
        kind = program.get("kind", "LAD")
        if program.get("language") != "LD":
            raise Refuse(f"{path}: POU {program.get('title')} is {program.get('language')}, not LD")
        name = "MAIN" if index == 0 else str(program.get("title", f"POU{index}")).upper()
        pou = Pou(name, "functionBlock" if kind == "FB" else "program", source)
        pou.dry = True
        for rung in program.get("rungs") or []:
            pou.emit(rung)
        pou.dry = False
        pou.lines = []
        pou.next_id = 10
        for rung in program.get("rungs") or []:
            pou.emit(rung)
        pous.append(pou)

    types = "<types><dataTypes>%s</dataTypes></types>" % "".join(
        f'<dataType name="{name}"><struct>%s</struct></dataType>'
        % "".join(
            f'<variable name="{field}"><type>{type_body(ty, source)}</type></variable>'
            for field, ty in fields.items()
        )
        for name, fields in source.structs.items()
    ) if source.structs else "<types><dataTypes/></types>"

    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<project xmlns="http://www.plcopen.org/xml/tc6_0201">',
        '<fileHeader company="converted" product="plcy2plcopen" version="1"/>',
        f'<contentHeader name="{source.stem}"/>',
        types,
        "<instances><configurations/></instances>",
        "<addData/>",
        "<pous>",
    ]
    for pou in pous:
        lines.append(f'<pou name="{pou.name}" pouType="{pou.kind}">')
        lines.append(pou.interface_xml())
        lines.append("<body><LD>")
        lines.append('<leftPowerRail localId="0"/>')
        lines.extend(pou.lines)
        lines.append('<rightPowerRail localId="9999"/>')
        lines.append("</LD></body></pou>")
    lines.append("</pous></project>")
    return "\n".join(lines) + "\n"


def main(argv):
    def write(target, text):
        try:
            with open(target, "w") as handle:
                handle.write(text)
        except OSError as error:
            raise Refuse(f"{target}: {error}") from error

    if len(argv) == 2 and os.path.isdir(argv[0]):
        out = argv[1]
        try:
            os.makedirs(out, exist_ok=True)
            names = sorted(os.listdir(argv[0]))
        except OSError as error:
            raise Refuse(f"{out}: {error}") from error
        for name in names:
            if not name.endswith(".plcy"):
                continue
            source = os.path.join(argv[0], name)
            target = os.path.join(out, os.path.splitext(name)[0] + ".xml")
            write(target, convert(source))
            print(f"{source} -> {target}")
        return 0
    if len(argv) == 2:
        write(argv[1], convert(argv[0]))
        return 0
    print(__doc__.strip().splitlines()[-1], file=sys.stderr)
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Refuse as refusal:
        print(f"plcy2plcopen: {refusal}", file=sys.stderr)
        sys.exit(1)
