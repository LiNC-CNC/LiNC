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

use crate::model::{Coil, CoilKind, IoDirection, IoPoint, Logic, Pou, PouLanguage, Project, Rung, Section, VarDecl};
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

/// The model accepts `^[a-z][a-z0-9_-]{0,31}$`, while PLCopen names are any
/// identifier, usually capitalised. Import lowercases and replaces the rest, so
/// `Motor` becomes `motor`; a collision then shows up as `duplicate_symbol`
/// rather than as a silent merge.
fn operand_name(raw: &str) -> String {
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
                    element
                        .text
                        .push_str(&decoded.unwrap_or_else(|_| String::from_utf8_lossy(&text).to_string()));
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

    let mut project = Project::new(name);
    project.schema = SCHEMA_VERSION;

    let mut pous = Vec::new();
    let Some(types) = project_element.child("types") else {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            "the file has no <types> section",
            "project",
        ));
        return Err(diagnostics);
    };
    let Some(pous_element) = types.child("pous") else {
        diagnostics.push(error(
            "plcopen_unsupported_element",
            "the file has no <pous> section",
            "types",
        ));
        return Err(diagnostics);
    };

    let mut seen_main = false;
    for (index, pou_element) in pous_element.children_named("pou").enumerate() {
        let path = format!("pous[{index}]");
        if let Some((mut pou, io, vars)) = import_pou(pou_element, &path, &mut diagnostics) {
            // The first program is the entry point, as the repository's
            // model expects MAIN to be.
            if !seen_main
                && pou_element.attribute("pouType").unwrap_or("program") == "program"
            {
                pou.name = "MAIN".to_string();
                seen_main = true;
            }
            project.io.extend(io);
            project.vars.extend(vars);
            pous.push(pou);
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
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ImportedPou> {
    let name = element
        .attribute("name")
        .unwrap_or("POU")
        .to_string();
    let mut pou = Pou::new(name, PouLanguage::Ld);
    let mut io = Vec::new();
    let mut vars = Vec::new();

    if let Some(interface) = element.child("interface") {
        import_interface(interface, path, &mut io, &mut vars, diagnostics);
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

    match import_ld(ld, path, &mut pou) {
        Ok(()) => Some((pou, io, vars)),
        Err(more) => {
            diagnostics.extend(more);
            None
        }
    }
}

fn import_interface(
    interface: &Element,
    path: &str,
    io: &mut Vec<IoPoint>,
    vars: &mut Vec<VarDecl>,
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
            if !is_bool(variable) {
                diagnostics.push(error(
                    "plcopen_unsupported_element",
                    format!("variable '{name}' is not BOOL"),
                    format!("{path}.interface.{name}"),
                ));
                continue;
            }
            let name = operand_name(name);
            match direction {
                Some(direction) => io.push(IoPoint::new(name, direction)),
                None => vars.push(VarDecl::new(name)),
            }
        }
    }
}

fn is_bool(variable: &Element) -> bool {
    variable
        .child("type")
        .map(|kind| kind.child("BOOL").is_some())
        .unwrap_or(false)
}

/// Markup in `<documentation>` is stripped: the model stores plain text.
fn plain_text(element: &Element) -> String {
    let mut text = element.text.clone();
    for child in &element.children {
        text.push_str(&plain_text(child));
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn import_ld(ld: &Element, path: &str, pou: &mut Pou) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let mut coils = Vec::new();
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
                if child.name == "coil" {
                    coils.push(id.clone());
                }
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
    if coils.is_empty() {
        diagnostics.push(error(
            "plcopen_unreachable",
            "the ladder has no <coil>, so there is no rung to rebuild",
            format!("{path}.body.LD"),
        ));
        return Err(diagnostics);
    }

    let mut section = Section::new("main");
    for (index, coil_id) in coils.iter().enumerate() {
        let coil_element = elements[coil_id];
        let Some(variable) = coil_element.child("variable") else {
            diagnostics.push(error(
                "plcopen_unsupported_element",
                "a <coil> has no <variable>",
                format!("{path}.body.LD.coil[{index}]"),
            ));
            continue;
        };
        let Some(operand) = operand_text(variable) else {
            diagnostics.push(error(
                "plcopen_unsupported_element",
                "a <coil> names no operand",
                format!("{path}.body.LD.coil[{index}]"),
            ));
            continue;
        };
        let mut visiting = HashSet::new();
        let logic = match resolve_input(
            coil_element,
            &elements,
            &left_rails,
            &right_rails,
            &mut visiting,
            &format!("{path}.body.LD.coil[{index}]"),
            &mut diagnostics,
        ) {
            Ok(logic) => logic,
            Err(()) => continue,
        };
        let kind = coil_kind(coil_element);
        let mut rung = Rung::new(logic, Coil::new(operand, kind));
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

/// `<variable>` may carry the name as text (the usual LD form) or as an
/// `@name`; both are the same operand.
fn operand_text(variable: &Element) -> Option<String> {
    if let Some(name) = variable.attribute("name") {
        return Some(operand_name(name));
    }
    let text = variable.text.trim();
    if text.is_empty() {
        None
    } else {
        Some(operand_name(text))
    }
}

fn coil_kind(coil: &Element) -> CoilKind {
    let negated = coil
        .attribute("negated")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    match coil.attribute("storage").unwrap_or("none") {
        "set" => CoilKind::Set,
        "reset" => CoilKind::Reset,
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
            "contact" => build_contact(source, path, diagnostics),
            "coil" => {
                diagnostics.push(error(
                    "plcopen_multi_coil",
                    "a coil feeds another coil; a rung has exactly one coil",
                    path,
                ));
                return Err(());
            }
            "block" => build_block(source, elements, left_rails, right_rails, visiting, path, diagnostics),
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
            path,
            diagnostics,
        )?;
        visiting.remove(&reference);
        let combined = match (upstream, inner) {
            (Logic::Const { value: true }, inner) => inner,
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
    let Some(operand) = operand_text(variable) else {
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
    Ok(Logic::Block {
        block: name,
        instance: String::new(),
        preset,
    })
}
