//! PLCopen TC6 / IEC 61131-10 XML, imported into this repository's model.
//!
//! Written from the published standard: element and attribute names are format
//! facts. Nothing here is derived from an implementation's source.
//!
//! The importer is deliberately narrow and all-or-nothing. It reads ladder
//! bodies, maps them to `Logic` trees (never to grid cells, so no geometry is
//! invented), and refuses the whole file if any construct has no faithful
//! mapping. A partial ladder must never reach a live PLC.
//!
//! Accepted shape, in the standard's names:
//!
//! ```text
//! project@xmlns, fileHeader, contentHeader@name, types/pous/pou
//!   pou@pouType=program|functionBlock|function, @name
//!   interface/localVars|inputVars|outputVars/variable@name/type/BOOL
//!   body/LD/leftPowerRail|rightPowerRail|contact|coil|block
//! ```
//!
//! Connections are followed backwards from each coil: one `connection` is a
//! series step, several are a parallel branch, and the walk ends at the left
//! power rail, which is `Const true`.

use std::collections::{HashMap, HashSet};

use crate::model::{
    Coil, CoilKind, IoDirection, IoKind, IoPoint, Logic, Pou, PouLanguage, Project, Rung, Section,
    VarDecl, VarType,
};
use crate::validate::Diagnostic;
use crate::SCHEMA_VERSION;

/// One parsed XML element. A small tree is enough for a ladder and keeps the
/// "refuse what is not understood" rule in one place.
#[derive(Debug, Clone, Default)]
struct Element {
    name: String,
    attributes: HashMap<String, String>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |child| child.name == name)
    }

    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }
}

/// The model accepts `^[a-z][a-z0-9_-]{0,31}$`, while an imported name may be
/// anything. Lowercase it and replace the rest, so `Motor` becomes `motor`; a
/// collision then shows up as `duplicate_symbol` rather than as a silent merge.
/// The house rule for an imported identifier: lowercase, `_` for anything the
/// model's name pattern rejects, `n_` when it would start with a digit, 32
/// characters at most. `clp` uses it too, so the two importers sanitise alike.
pub fn operand_name(raw: &str) -> String {
    let mut name = String::with_capacity(raw.len());
    for character in raw.trim().chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            name.push(character.to_ascii_lowercase());
        } else {
            name.push('_');
        }
    }
    if name.is_empty() {
        name.push('n');
    }
    if !name.starts_with(|character: char| character.is_ascii_lowercase()) {
        name.insert_str(0, "n_");
    }
    name.truncate(32);
    name
}

fn error(code: &str, message: impl Into<String>, path: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, message, path)
}

/// Read the file into a tree, keeping the local names (`<tns:contact>` and
/// `<contact>` are the same element to a reader of the standard).
fn parse_xml(text: &str) -> Result<Element, Diagnostic> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Element> = vec![Element {
        name: "#document".to_string(),
        ..Element::default()
    }];
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let mut element = Element {
                    name: local_name(&start),
                    ..Element::default()
                };
                for attribute in start.attributes().flatten() {
                    let key = local_name_bytes(attribute.key.as_ref());
                    let value = attribute
                        .decode_and_unescape_value(reader.decoder())
                        .map(|value| value.to_string())
                        .unwrap_or_default();
                    element.attributes.insert(key, value);
                }
                stack.push(element);
            }
            Ok(Event::Empty(empty)) => {
                let mut element = Element {
                    name: local_name(&empty),
                    ..Element::default()
                };
                for attribute in empty.attributes().flatten() {
                    let key = local_name_bytes(attribute.key.as_ref());
                    let value = attribute
                        .decode_and_unescape_value(reader.decoder())
                        .map(|value| value.to_string())
                        .unwrap_or_default();
                    element.attributes.insert(key, value);
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            }
            Ok(Event::End(_)) => {
                if stack.len() > 1 {
                    let element = stack.pop().expect("a started element is on the stack");
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(element);
                    }
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(element) = stack.last_mut() {
                    let decoded = text.unescape().map(|value| value.to_string());
                    element.text.push_str(
                        &decoded.unwrap_or_else(|_| String::from_utf8_lossy(&text).to_string()),
                    );
                }
            }
            Ok(Event::CData(data)) => {
                if let Some(element) = stack.last_mut() {
                    element.text.push_str(&String::from_utf8_lossy(&data));
                }
            }
            Ok(Event::Eof) => break,
            Err(failure) => {
                return Err(error(
                    "plcopen_xml",
                    format!("the file is not well-formed XML: {failure}"),
                    "xml",
                ))
            }
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(error(
            "plcopen_xml",
            "the file ends with unclosed elements",
            "xml",
        ));
    }
    stack
        .pop()
        .ok_or_else(|| error("plcopen_xml", "the file has no root element", "xml"))
}

fn local_name(start: &quick_xml::events::BytesStart<'_>) -> String {
    local_name_bytes(start.name().as_ref())
}

fn local_name_bytes(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match text.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => text.to_string(),
    }
}

/// Import a PLCopen TC6 file. `Err` carries every diagnostic found, and no
/// project: the caller must not load a partial ladder.
pub fn import(xml: &str) -> Result<Project, Vec<Diagnostic>> {
    let root = match parse_xml(xml) {
        Ok(root) => root,
        Err(diagnostic) => return Err(vec![diagnostic]),
    };
    let mut diagnostics = Vec::new();
    let project_element = match root.children.iter().find(|child| child.name == "project") {
        Some(project) => project,
        None => {
            diagnostics.push(error(
                "plcopen_unsupported_element",
                "the root element is not <project>",
                "xml",
            ));
            return Err(diagnostics);
        }
    };

    let name = project_element
        .child("contentHeader")
        .and_then(|header| header.attribute("name"))
        .unwrap_or("imported")
        .to_string();

    // One instance per block of a kind, across the whole file.
    let mut instances: HashMap<crate::profile::spec::FbName, u16> = HashMap::new();

    let mut project = Project::new(name);
    project.schema = SCHEMA_VERSION;

    let mut pous = Vec::new();
    // Two shapes are in use, and both are TC6: 0200 puts the POUs inside
    // `<types>`, 0201 allows them directly under `<project>`. Read either.
    let pous_element = project_element
        .child("types")
        .and_then(|types| types.child("pous"))
        .or_else(|| project_element.child("pous"));
    let Some(pous_element) = pous_element else {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            "the file has no <pous> section, under <types> or <project>",
            "project",
        ));
        return Err(diagnostics);
    };

    let mut seen_main = false;
    for (index, pou_element) in pous_element.children_named("pou").enumerate() {
        let path = format!("pous[{index}]");
        if let Some((mut pou, io, vars)) =
            import_pou(pou_element, &path, &mut instances, &mut diagnostics)
        {
            // The first program is the entry point, as the repository's
            // model expects MAIN to be.
            if !seen_main && pou_element.attribute("pouType").unwrap_or("program") == "program" {
                pou.name = "MAIN".to_string();
                seen_main = true;
            }
            if pou.kind == crate::model::PouKind::FunctionBlock {
                // A block's interface is the block's: its instances each get their
                // own copy, so nothing here becomes a project variable.
                pou.vars = vars;
            } else {
                project.io.extend(io);
                project.vars.extend(vars);
            }
            pous.push(pou);
        }
    }

    // The project's own types: a `<dataType>` holding a `<struct>` is a struct whose
    // fields share one word type, which is what makes its elements addressable.
    if let Some(types) = project_element
        .child("types")
        .and_then(|types| types.child("dataTypes"))
    {
        for data_type in types.children_named("dataType") {
            let Some(name) = data_type.attribute("name") else {
                continue;
            };
            let Some(block) = data_type.child("struct") else {
                continue;
            };
            let mut fields = Vec::new();
            let mut readable = true;
            for variable in block.children_named("variable") {
                let Some(field) = variable.attribute("name") else {
                    continue;
                };
                let Some(ty) = declared_type(variable) else {
                    diagnostics.push(error(
                        "plcopen_unsupported_element",
                        format!("struct '{name}' field '{field}' has a type with no LiNC form"),
                        format!("types.{name}.{field}"),
                    ));
                    readable = false;
                    continue;
                };
                fields.push(VarDecl::typed(operand_name(field), ty));
            }
            if readable && !fields.is_empty() {
                project.structs.push(crate::model::StructDecl {
                    name: name.to_string(),
                    fields,
                });
            }
        }
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    if pous.is_empty() {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            "the file declares no program or function block",
            "types/pous",
        ));
        return Err(diagnostics);
    }
    project.pous = pous;
    Ok(project)
}

type ImportedPou = (Pou, Vec<IoPoint>, Vec<VarDecl>);

fn import_pou(
    element: &Element,
    path: &str,
    instances: &mut HashMap<crate::profile::spec::FbName, u16>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ImportedPou> {
    let name = element.attribute("name").unwrap_or("POU").to_string();
    let mut pou = Pou::new(name, PouLanguage::Ld);
    let mut io = Vec::new();
    let mut vars = Vec::new();

    let function_block = element
        .attribute("pouType")
        .is_some_and(|kind| kind.eq_ignore_ascii_case("functionBlock"));
    if function_block {
        pou.kind = crate::model::PouKind::FunctionBlock;
    }
    if let Some(interface) = element.child("interface") {
        import_interface_of(
            interface,
            path,
            &mut io,
            &mut vars,
            function_block,
            diagnostics,
        );
    }
    if let Some(documentation) = element.child("documentation") {
        pou.comment = plain_text(documentation);
    }

    let Some(body) = element.child("body") else {
        diagnostics.push(error(
            "plcopen_unsupported_body",
            format!("POU '{}' has no body", pou.name),
            format!("{path}.body"),
        ));
        return None;
    };
    let Some(ld) = body.child("LD") else {
        let kind = body
            .children
            .iter()
            .map(|child| child.name.as_str())
            .next()
            .unwrap_or("empty");
        diagnostics.push(error(
            "plcopen_unsupported_body",
            format!(
                "POU '{}' has a <{kind}> body; only <LD> can be imported",
                pou.name
            ),
            format!("{path}.body"),
        ));
        return None;
    };

    // A variable whose type names a function block is an instance: its fields are
    // operands (`Station.Filling`), so the body import has to know which names are
    // instances and not flattened into a single identifier.
    let instances_declared: HashSet<String> = vars
        .iter()
        .filter(|var| matches!(var.ty, crate::model::VarType::Derived(_)))
        .map(|var| var.name.to_ascii_lowercase())
        .collect();
    match import_ld(ld, path, &mut pou, instances, &instances_declared) {
        Ok(()) => Some((pou, io, vars)),
        Err(more) => {
            diagnostics.extend(more);
            None
        }
    }
}

/// Read one `<interface>`. A function block's variables are its own — its pins and
/// its private state — so they are declarations with a direction, never HAL points.
fn import_interface_of(
    interface: &Element,
    path: &str,
    io: &mut Vec<IoPoint>,
    vars: &mut Vec<VarDecl>,
    function_block: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for section in &interface.children {
        let direction = match section.name.as_str() {
            "localVars" => None,
            "inputVars" => Some(IoDirection::In),
            "outputVars" => Some(IoDirection::Out),
            other => {
                diagnostics.push(error(
                    "plcopen_unsupported_element",
                    format!("<{other}> in the interface has no mapping"),
                    format!("{path}.interface.{other}"),
                ));
                continue;
            }
        };
        for variable in section.children_named("variable") {
            let Some(name) = variable.attribute("name") else {
                diagnostics.push(error(
                    "plcopen_unsupported_element",
                    "a <variable> has no name",
                    format!("{path}.interface"),
                ));
                continue;
            };
            let Some(ty) = declared_type(variable) else {
                diagnostics.push(error(
                    "plcopen_unsupported_element",
                    format!("variable '{name}' has a type with no LiNC form"),
                    format!("{path}.interface.{name}"),
                ));
                continue;
            };
            // The declaration's own spelling before the importer renames it: an
            // address (`%I0.0`) is the binding an alias sits on top of.
            let declared = name.trim().to_string();
            let name = operand_name(name);
            if function_block {
                // A pin is a declaration with a direction; an internal has neither.
                vars.push(VarDecl {
                    direction,
                    ..VarDecl::typed(name, ty)
                });
                continue;
            }
            match direction {
                Some(direction) => {
                    // A point's kind decides its HAL pin: a bit is a coil's
                    // contact, a word is an `s32` pin, a float is a `float` pin the
                    // scan sees as its integer view.
                    let Some(kind) = point_kind(&ty) else {
                        diagnostics.push(error(
                            "plcopen_unsupported_element",
                            format!(
                                "variable '{name}' is {} and cannot be an I/O point; \
                                 a point is BOOL, INT, DINT, REAL or TIME",
                                ty.label()
                            ),
                            format!("{path}.interface.{name}"),
                        ));
                        continue;
                    };
                    // The declaration's own spelling is the binding: `name` is the
                    // operand the ladder reads, `address` the address it stands for.
                    let address = if declared.starts_with('%') {
                        declared.clone()
                    } else {
                        String::new()
                    };
                    io.push(IoPoint {
                        name,
                        direction,
                        kind,
                        comment: String::new(),
                        address,
                    });
                }
                None => {
                    let mut decl = VarDecl::typed(name, ty);
                    decl.init = initial_values(variable, path, diagnostics);
                    vars.push(decl);
                }
            }
        }
    }
}

/// The declared type of a `<variable>`, or `None` when the element has no `<type>`
/// or names one this build cannot read.
fn declared_type(variable: &Element) -> Option<VarType> {
    read_type(variable.child("type")?)
}

/// Read a type element: `<BOOL/>`, `<INT/>`, `<DINT/>`, `<REAL/>`, `<TIME/>`,
/// `<derived name="X"/>`, or `<array>` over one of them.
///
/// A derived type is *named*, not resolved: its declaration is the project's own,
/// and a refusal that says `MotorStarter` is worth more than one that says
/// "unknown type". A type element with more than one child, or a name this build
/// has no width for, answers `None` and the caller reports it.
fn read_type(kind: &Element) -> Option<VarType> {
    let mut children = kind.children.iter();
    let child = children.next()?;
    if children.next().is_some() {
        return None;
    }
    match child.name.as_str() {
        "BOOL" => Some(VarType::Bool),
        "INT" => Some(VarType::Int),
        "DINT" => Some(VarType::Dint),
        "REAL" => Some(VarType::Real),
        "TIME" => Some(VarType::Time),
        "derived" => child
            .attribute("name")
            .map(|name| VarType::Derived(name.to_string())),
        // `<array>` holds the element type and its bounds; the bounds are the
        // standard's inclusive `lower`/`upper`, and the bank indexes from zero.
        "array" => {
            let base = read_type(child.child("baseElementType")?)?;
            let dimension = child.child("dimension")?;
            let bound = |name: &str| {
                dimension
                    .attribute(name)
                    .and_then(|text| text.trim().parse::<i64>().ok())
            };
            let lower = bound("lower").unwrap_or(1);
            let upper = bound("upper")?;
            let len = upper.checked_sub(lower)?.checked_add(1)?;
            if len <= 0 {
                return None;
            }
            Some(VarType::Array {
                base: Box::new(base),
                len: u32::try_from(len).ok()?,
            })
        }
        _ => None,
    }
}

/// A declaration's `<initialValue>`: the numbers a word variable starts with.
///
/// The standard writes a scalar as `<simpleValue value="5"/>` or `<value>5</value>`,
/// and an array as `<arrayValue>` around the element values. A value that is not a
/// number is refused by name rather than skipped: a declaration whose initial value
/// this build cannot read would silently start at zero.
fn initial_values(variable: &Element, path: &str, diagnostics: &mut Vec<Diagnostic>) -> Vec<i32> {
    let Some(initial) = variable.child("initialValue") else {
        return Vec::new();
    };
    let mut values = Vec::new();
    collect_values(initial, &mut values);
    let mut numbers = Vec::with_capacity(values.len());
    for value in values {
        match value.trim().parse::<i32>() {
            Ok(number) => numbers.push(number),
            Err(_) => {
                let name = variable.attribute("name").unwrap_or("?");
                diagnostics.push(error(
                    "plcopen_unsupported_element",
                    format!(
                        "variable '{name}' has an initial value '{value}' that is not a number"
                    ),
                    format!("{path}.interface.{name}"),
                ));
                return Vec::new();
            }
        }
    }
    numbers
}

/// Collect the `<value>` texts and `value` attributes under one initial value.
fn collect_values(element: &Element, values: &mut Vec<String>) {
    if let Some(attribute) = element.attribute("value") {
        values.push(attribute.to_string());
    }
    for child in &element.children {
        match child.name.as_str() {
            "value" | "simpleValue" => {
                if let Some(attribute) = child.attribute("value") {
                    values.push(attribute.to_string());
                } else if !child.text.trim().is_empty() {
                    values.push(child.text.trim().to_string());
                }
            }
            _ => collect_values(child, values),
        }
    }
}

/// The HAL pin an I/O point of this type uses. An array or a structured type is not
/// a point: a pin carries one value.
fn point_kind(ty: &VarType) -> Option<IoKind> {
    match ty {
        VarType::Bool => Some(IoKind::Bit),
        VarType::Int | VarType::Dint | VarType::Time => Some(IoKind::Word),
        VarType::Real => Some(IoKind::Float),
        VarType::Derived(_) | VarType::Array { .. } => None,
    }
}

/// Markup in `<documentation>` is stripped: the model stores plain text.
fn plain_text(element: &Element) -> String {
    let mut text = element.text.clone();
    for child in &element.children {
        text.push_str(&plain_text(child));
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn import_ld(
    ld: &Element,
    path: &str,
    pou: &mut Pou,
    instances: &mut HashMap<crate::profile::spec::FbName, u16>,
    declared: &HashSet<String>,
) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let mut order = Vec::new();
    let mut elements: HashMap<String, &Element> = HashMap::new();
    let mut left_rails = HashSet::new();
    let mut right_rails = HashSet::new();

    for child in &ld.children {
        let id = child.attribute("localId").map(str::to_string);
        match child.name.as_str() {
            "leftPowerRail" => {
                if let Some(id) = id {
                    left_rails.insert(id);
                }
            }
            "rightPowerRail" => {
                if let Some(id) = id {
                    right_rails.insert(id);
                }
            }
            "contact" | "coil" | "block" => {
                let Some(id) = id else {
                    diagnostics.push(error(
                        "plcopen_unreachable",
                        format!("a <{}> has no localId, so no wire can name it", child.name),
                        format!("{path}.body.LD"),
                    ));
                    continue;
                };
                order.push(id.clone());
                elements.insert(id, child);
            }
            "comment" | "documentation" => {}
            other => diagnostics.push(error(
                "plcopen_unsupported_element",
                format!("<{other}> in an LD body has no mapping"),
                format!("{path}.body.LD.{other}"),
            )),
        }
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    // A rung ends where nothing consumes the wire: at a coil, always, and at a
    // block that drives nothing. The second kind is a rung in IEC 61131-3 —
    // `TON` on a condition with no output — and walking back only from coils
    // dropped it, which made a program run without its timer.
    let mut consumed = HashSet::new();
    for id in &order {
        for source in connection_refs(elements[id]) {
            consumed.insert(source);
        }
    }
    let terminators = order
        .iter()
        .filter(|id| !consumed.contains(*id) && elements[*id].name != "contact")
        .cloned()
        .collect::<Vec<_>>();
    if terminators.is_empty() {
        diagnostics.push(error(
            "plcopen_unreachable",
            "the ladder has no <coil> and no block that ends a rung",
            format!("{path}.body.LD"),
        ));
        return Err(diagnostics);
    }

    // The section is named after its POU: a file with several POUs has several
    // bodies, and a section name has to be unique across the project.
    let mut section = Section::new(operand_name(pou.name.as_str()));
    for (index, terminator) in terminators.iter().enumerate() {
        let end = elements[terminator];
        if end.name == "block" {
            // A block that drives nothing ends the rung all the same: the rung is
            // what feeds it, series with the block itself. `resolve_input` returns
            // the feeding side only, which is right for a coil (the coil is the
            // rung's own field) and wrong here.
            let path = format!("{path}.body.LD.block[{index}]");
            let mut visiting = HashSet::new();
            let feeding = match resolve_input(
                end,
                &elements,
                &left_rails,
                &right_rails,
                &mut visiting,
                instances,
                declared,
                &path,
                &mut diagnostics,
            ) {
                Ok(logic) => logic,
                Err(()) => continue,
            };
            let block = match build_block(
                end,
                &elements,
                &left_rails,
                &right_rails,
                &mut HashSet::new(),
                instances,
                &path,
                &mut diagnostics,
            ) {
                Ok(logic) => logic,
                Err(()) => continue,
            };
            // A block consumes the power bit and pushes its `Q`, so the rung keeps
            // the bit that feeds it even when that bit is the rail's true: a bare
            // block would be handed an empty stack and never see its input high.
            let logic = Logic::series(vec![feeding, block]);
            let mut rung = Rung::without_coil(logic);
            if let Some(documentation) = end.child("documentation") {
                rung.comment = plain_text(documentation);
            }
            section.rungs.push(rung);
            continue;
        }
        let coil_element = end;
        let kind = coil_kind(coil_element);
        // A compute coil carries an assignment instead of a bit: `<expression>`
        // holds `target = value`, and the coil has no `<variable>`.
        let assignment = if kind == CoilKind::Compute {
            let text = coil_element
                .child("expression")
                .map(|expression| sanitise_addresses(&plain_text(expression)))
                .unwrap_or_default();
            if text.is_empty() {
                diagnostics.push(error(
                    "plcopen_unsupported_element",
                    "a compute coil has no <expression>",
                    format!("{path}.body.LD.coil[{index}]"),
                ));
                continue;
            }
            Some(text)
        } else {
            None
        };
        let operand = match &assignment {
            Some(_) => String::new(),
            None => {
                let Some(variable) = coil_element.child("variable") else {
                    diagnostics.push(error(
                        "plcopen_unsupported_element",
                        "a <coil> has no <variable>",
                        format!("{path}.body.LD.coil[{index}]"),
                    ));
                    continue;
                };
                let Some(operand) = operand_text_of(variable, declared) else {
                    diagnostics.push(error(
                        "plcopen_unsupported_element",
                        "a <coil> names no operand",
                        format!("{path}.body.LD.coil[{index}]"),
                    ));
                    continue;
                };
                operand
            }
        };
        let mut visiting = HashSet::new();
        let logic = match resolve_input(
            coil_element,
            &elements,
            &left_rails,
            &right_rails,
            &mut visiting,
            instances,
            declared,
            &format!("{path}.body.LD.coil[{index}]"),
            &mut diagnostics,
        ) {
            Ok(logic) => logic,
            Err(()) => continue,
        };
        let mut coil = Coil::new(operand, kind);
        coil.expr = assignment;
        if kind == CoilKind::Call {
            coil.args = call_args(coil_element);
        }
        let mut rung = Rung::new(logic, coil);
        if let Some(documentation) = coil_element.child("documentation") {
            rung.comment = plain_text(documentation);
        }
        section.rungs.push(rung);
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    pou.sections = vec![section];
    Ok(())
}

/// Rewrite the `%` addresses inside an expression to the names the interface declares.
///
/// A compute coil's expression is an expression, not one operand, so its addresses have
/// to be spelled the way the symbols are before the expression engine sees them.
fn sanitise_addresses(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token.is_empty() {
            return;
        }
        if token.starts_with('%') && token.len() > 1 {
            out.push_str(&operand_name(token));
        } else {
            out.push_str(token);
        }
        token.clear();
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric()
            || character == '%'
            || character == '.'
            || character == '_'
        {
            token.push(character);
        } else {
            flush(&mut token, &mut out);
            out.push(character);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// `<variable>` may carry the name as text (the usual LD form) or as an
/// `@name`; both are the same operand.
fn operand_text_of(variable: &Element, instances: &HashSet<String>) -> Option<String> {
    let raw = match variable.attribute("name") {
        Some(name) => name.to_string(),
        None => variable.text.trim().to_string(),
    };
    if raw.is_empty() {
        return None;
    }
    Some(operand_spelling_of(&raw, instances))
}

/// A bit of a word keeps its shape: `Bits[0].0` names bit 0 of element 0 of `Bits`,
/// so the *word* is what a declaration has to match and the bit stays a number. The
/// word is spelled the way its declaration is, and a marker word keeps the profile's
/// own spelling.
///
/// A block member keeps its dot: `TON0.Q` names a member of an instance, not a
/// symbol, and [`operand_name`] would flatten it into `ton0_q`. The instance part
/// is uppercase because that is how the profile spells a block, and the member is
/// left as written because member matching is case-insensitive.
///
/// Anything else goes through [`operand_name`], including a `%Q0.0` tag: the part
/// after the dot there is a number, not a member.
pub fn operand_spelling(raw: &str) -> String {
    operand_spelling_of(raw, &HashSet::new())
}

/// `operand_spelling`, knowing which names are declared instances: `Station.Filling`
/// is a field of instance `Station`, so it keeps its dot and its spelling the way a
/// block member does.
pub fn operand_spelling_of(raw: &str, instances: &HashSet<String>) -> String {
    let raw = raw.trim();
    if let Some((base, field)) = raw.split_once('.') {
        if !field.is_empty()
            && field
                .chars()
                .all(|character| character.is_ascii_alphabetic())
            && instances.contains(&base.to_ascii_lowercase())
        {
            return format!("{}.{}", operand_name(base), field.to_ascii_lowercase());
        }
    }
    // One field of one element of an array of structs, `Recipes[Sel].FillTime`: the
    // array, the index and the field are names, so each is spelled the way its
    // declaration is and the brackets stay.
    if let Some(operand) = crate::model::ArrayField::parse(raw) {
        return format!(
            "{}[{}].{}",
            operand_name(&operand.array),
            array_index_spelling(&operand.index),
            operand_name(&operand.field)
        );
    }
    // An array element read, `Durations[Phase]`: the array and the index are both
    // declared names, so both are spelled the way their declarations are and the
    // bracket stays. A literal index stays a number.
    if let Some(operand) = crate::model::ArrayElement::parse(raw) {
        return format!(
            "{}[{}]",
            operand_name(&operand.array),
            array_index_spelling(&operand.index)
        );
    }
    if let Some(operand) = crate::model::WordBit::parse(raw) {
        if crate::profile::resolve::marker_word_slot(&operand.word).is_some() {
            // The marker word's spelling is the profile's, and this form is one.
            return raw.to_string();
        }
        if !operand.word.starts_with('%') {
            // No `%` prefix means a declared name, which the interface spells the way
            // `operand_name` does. A `%`-prefixed word is an address, and the address
            // path below owns those.
            let word = operand_name(&operand.word);
            return match operand.element {
                Some(element) => format!("{word}[{element}].{}", operand.bit),
                None => format!("{word}.{}", operand.bit),
            };
        }
    }
    if let Some((base, member)) = raw.split_once('.') {
        let instance = !base.is_empty()
            && base.chars().any(|character| character.is_ascii_digit())
            && base
                .chars()
                .all(|character| character.is_ascii_alphanumeric());
        let named = !member.is_empty()
            && member
                .chars()
                .all(|character| character.is_ascii_alphabetic());
        if instance && named {
            return format!("{}.{}", base.to_ascii_uppercase(), member);
        }
    }
    operand_name(raw)
}

/// The index of an array element: a declared name spelled the way its declaration
/// is, or a literal left as it was written.
fn array_index_spelling(index: &str) -> String {
    let index = index.trim();
    if index.parse::<i64>().is_ok() {
        return index.to_string();
    }
    operand_name(index)
}

/// The pins one call binds: `<inputVariables>` are written before the body runs and
/// `<outputVariables>` are read after it.
fn call_args(coil: &Element) -> Vec<crate::model::CallArg> {
    let mut args = Vec::new();
    for (section, out) in [("inputVariables", false), ("outputVariables", true)] {
        let Some(inputs) = coil.child(section) else {
            continue;
        };
        for variable in inputs.children_named("variable") {
            let Some(pin) = variable.attribute("formalParameter") else {
                continue;
            };
            // The bound operand is an expression so a bit address and a name are
            // spelled the same way here.
            let Some(value) = variable
                .child("expression")
                .map(|expression| operand_spelling(&plain_text(expression)))
            else {
                continue;
            };
            args.push(crate::model::CallArg {
                pin: operand_name(pin),
                var: value,
                out,
            });
        }
    }
    args
}

fn coil_kind(coil: &Element) -> CoilKind {
    let negated = coil
        .attribute("negated")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    match coil.attribute("storage").unwrap_or("none") {
        "set" => CoilKind::Set,
        "reset" => CoilKind::Reset,
        // LiNC's own two coil forms. A call names a POU and inlines it under the
        // rung's power; a compute coil carries an assignment in `<expression>`.
        "call" => CoilKind::Call,
        "compute" => CoilKind::Compute,
        _ if negated => CoilKind::Negated,
        _ => CoilKind::Normal,
    }
}

/// Walk `connectionPointIn` backwards. One connection is what feeds this
/// element, several are parallel branches, and the left rail is true.
fn resolve_input(
    element: &Element,
    elements: &HashMap<String, &Element>,
    left_rails: &HashSet<String>,
    right_rails: &HashSet<String>,
    visiting: &mut HashSet<String>,
    instances: &mut HashMap<crate::profile::spec::FbName, u16>,
    declared: &HashSet<String>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Logic, ()> {
    let refs = connection_refs(element);
    if refs.is_empty() {
        // A power rail that feeds nothing is true; anything else has no source.
        if element.name == "leftPowerRail" {
            return Ok(Logic::Const { value: true });
        }
        diagnostics.push(error(
            "plcopen_unreachable",
            format!("<{}> has no connection to a rail", element.name),
            path,
        ));
        return Err(());
    }
    let mut branches = Vec::new();
    for reference in refs {
        if left_rails.contains(&reference) {
            branches.push(Logic::Const { value: true });
            continue;
        }
        if right_rails.contains(&reference) {
            diagnostics.push(error(
                "plcopen_unsupported_element",
                "a connection comes from a right power rail",
                path,
            ));
            return Err(());
        }
        let Some(source) = elements.get(&reference) else {
            diagnostics.push(error(
                "plcopen_unreachable",
                format!("a connection names localId '{reference}', which is not in the body"),
                path,
            ));
            return Err(());
        };
        if !visiting.insert(reference.clone()) {
            diagnostics.push(error(
                "plcopen_cycle",
                format!("the connection through localId '{reference}' loops"),
                path,
            ));
            return Err(());
        }
        let built = match source.name.as_str() {
            "contact" => build_contact(source, path, declared, diagnostics),
            "coil" => {
                diagnostics.push(error(
                    "plcopen_multi_coil",
                    "a coil feeds another coil; a rung has exactly one coil",
                    path,
                ));
                return Err(());
            }
            "block" => build_block(
                source,
                elements,
                left_rails,
                right_rails,
                visiting,
                instances,
                path,
                diagnostics,
            ),
            other => {
                diagnostics.push(error(
                    "plcopen_unsupported_element",
                    format!("<{other}> cannot feed a rung"),
                    path,
                ));
                return Err(());
            }
        };
        let source_element = source;
        let inner = match built {
            Ok(logic) => logic,
            Err(()) => return Err(()),
        };
        // The element's own input feeds it; a contact is series with what came
        // before, which is expressed by nesting it in front.
        let upstream = resolve_input(
            source_element,
            elements,
            left_rails,
            right_rails,
            visiting,
            instances,
            declared,
            path,
            diagnostics,
        )?;
        visiting.remove(&reference);
        let combined = match (upstream, inner) {
            // A contact carries its own input, so a true bit in front of it says
            // nothing. A block does not: it consumes the power bit and pushes its
            // `Q`, so a block the rail feeds keeps the true bit in front of it or it
            // would be handed an empty stack.
            (Logic::Const { value: true }, inner) if !matches!(inner, Logic::Block { .. }) => inner,
            (upstream, Logic::Const { value: true }) => upstream,
            (upstream, inner) => Logic::series(vec![upstream, inner]),
        };
        branches.push(combined);
    }
    if branches.len() == 1 {
        Ok(branches.remove(0))
    } else {
        Ok(Logic::Parallel { items: branches })
    }
}

fn connection_refs(element: &Element) -> Vec<String> {
    let Some(point) = element.child("connectionPointIn") else {
        return Vec::new();
    };
    point
        .children_named("connection")
        .filter_map(|connection| connection.attribute("refLocalId"))
        .map(str::to_string)
        .collect()
}

fn build_contact(
    contact: &Element,
    path: &str,
    declared: &HashSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Logic, ()> {
    let Some(variable) = contact.child("variable") else {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            "a <contact> has no <variable>",
            path,
        ));
        return Err(());
    };
    let Some(operand) = operand_text_of(variable, declared) else {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            "a <contact> names no operand",
            path,
        ));
        return Err(());
    };
    match contact.attribute("edge") {
        Some(edge) if edge.eq_ignore_ascii_case("rising") => Ok(Logic::Edge {
            var: operand,
            rising: true,
        }),
        Some(edge) if edge.eq_ignore_ascii_case("falling") => Ok(Logic::Edge {
            var: operand,
            rising: false,
        }),
        Some(edge) => {
            diagnostics.push(error(
                "plcopen_unsupported_element",
                format!("contact edge '{edge}' is not rising or falling"),
                path,
            ));
            Err(())
        }
        None => Ok(Logic::Contact {
            var: operand,
            negated: contact
                .attribute("negated")
                .map(|value| value.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        }),
    }
}

fn build_block(
    block: &Element,
    elements: &HashMap<String, &Element>,
    left_rails: &HashSet<String>,
    right_rails: &HashSet<String>,
    visiting: &mut HashSet<String>,
    instances: &mut HashMap<crate::profile::spec::FbName, u16>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Logic, ()> {
    let Some(type_name) = block.attribute("typeName") else {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            "a <block> has no typeName",
            path,
        ));
        return Err(());
    };
    let name = type_name.to_ascii_uppercase();
    // An analogue block carries every operand as a pin, so all of its
    // `inputVariables` are read, and the one the table marks as the output is the
    // argument with `out` set.
    let analog = crate::profile::spec::spec(&name)
        .map(|spec| matches!(spec.family, crate::profile::spec::Family::Analog(_)))
        .unwrap_or(false);
    if analog {
        let mut args = Vec::new();
        for (section, out) in [("inputVariables", false), ("outputVariables", true)] {
            let Some(inputs) = block.child(section) else {
                continue;
            };
            for variable in inputs.children_named("variable") {
                let Some(pin) = variable.attribute("formalParameter") else {
                    continue;
                };
                let value = variable
                    .child("expression")
                    .map(|expression| {
                        let text = plain_text(expression);
                        // A controller member is a number, not a name: it is baked, so
                        // it keeps its spelling the way a preset literal does.
                        if text.trim().parse::<f32>().is_ok() {
                            text
                        } else {
                            operand_spelling(&text)
                        }
                    })
                    .unwrap_or_default();
                if value.is_empty() {
                    continue;
                }
                args.push(crate::model::CallArg {
                    pin: operand_name(pin),
                    var: value,
                    out,
                });
            }
        }
        // The destination is the pin the spec's right-hand side names.
        if let Some(spec) = crate::profile::spec::spec(&name) {
            if let Some(dest) = spec.pins.1.first() {
                if let Some(argument) = args
                    .iter_mut()
                    .find(|argument| argument.pin.eq_ignore_ascii_case(dest))
                {
                    argument.out = true;
                }
            }
        }
        // The controller tag is the block's instance: its state belongs to it.
        let instance = block
            .child("variable")
            .map(|variable| operand_text_of(variable, &HashSet::new()))
            .unwrap_or_default()
            .unwrap_or_default();
        return Ok(Logic::Block {
            block: name,
            instance,
            preset: String::new(),
            args,
        });
    }
    if crate::compile::compare_op(&name).is_some() {
        // A compare block carries its two values as inVariable expressions.
        let mut values = Vec::new();
        if let Some(inputs) = block.child("inputVariables") {
            for variable in inputs.children_named("variable") {
                if let Some(expression) = variable.child("expression") {
                    values.push(expression.text.trim().to_string());
                }
            }
        }
        if values.len() < 2 {
            diagnostics.push(error(
                "plcopen_unsupported_element",
                format!("compare block '{name}' needs two <expression> inputs"),
                path,
            ));
            return Err(());
        }
        return Ok(Logic::Compare {
            operator: name,
            left: values[0].clone(),
            right: values[1].clone(),
        });
    }
    if crate::compile::MathOp::parse(&name).is_some() {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            format!("compute block '{name}' has no faithful rung mapping"),
            path,
        ));
        return Err(());
    }
    if crate::compile::parse_block(&name, "", "").is_none() {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            format!("block '{name}' is not one this planner can run"),
            path,
        ));
        return Err(());
    }
    // Timer and counter presets live in the block's input variables as an
    // expression (`T#4s`, `10`), or as a connection to a literal.
    let mut preset = String::new();
    if let Some(inputs) = block.child("inputVariables") {
        for variable in inputs.children_named("variable") {
            let formal = variable.attribute("formalParameter").unwrap_or_default();
            if matches!(formal, "PT" | "PV") {
                if let Some(expression) = variable.child("expression") {
                    preset = expression.text.trim().to_string();
                }
            }
            if matches!(formal, "IN" | "CU" | "CD") {
                // The power path is the series connection; an explicit
                // connection here would be a second source this rung cannot
                // express, so refuse rather than silently ignore it.
                if variable.child("connectionPointIn").is_some() {
                    diagnostics.push(error(
                        "plcopen_unsupported_element",
                        format!("block '{name}' feeds {formal} from a wire"),
                        path,
                    ));
                    return Err(());
                }
            }
        }
    }
    let _ = (elements, left_rails, right_rails, visiting);
    // Each block of a kind gets its own instance. Binding every `<block>` to
    // instance 0 made two timers share one `FbState`, so the second overwrote
    // the first's preset and value every scan.
    let (kind, _, _) = crate::compile::parse_block(&name, "", "")
        .expect("the name was just checked against the table");
    let slot = instances.entry(kind.name()).or_insert(0u16);
    let number = *slot;
    if number >= crate::profile::FB_PER_TYPE {
        diagnostics.push(error(
            "plcopen_block_instances",
            format!(
                "block '{name}' is instance {number} of its kind; the scan carries {}",
                crate::profile::FB_PER_TYPE
            ),
            path,
        ));
        return Err(());
    }
    *slot += 1;
    Ok(Logic::Block {
        block: name,
        instance: crate::profile::resolve::block_instance(kind.name(), i32::from(number)),
        preset,
        args: Vec::new(),
    })
}
