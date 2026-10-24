//! Strict decoding of Probe Basic's numeric NML display messages.
//!
//! Only recognized widget updates are decoded. Message text is parsed as data,
//! never evaluated as code.

use linuxcnc_nml::MessageKind;

/// Extract a mapped parameter update from a Probe Basic display message.
///
/// Returns `None` for non-display messages, unknown widgets, malformed syntax,
/// and values that are not finite.
pub(crate) fn parse_probe_message(kind: MessageKind, text: &str) -> Option<(u32, f64)> {
    if !matches!(kind, MessageKind::OperatorDisplay | MessageKind::NmlDisplay) {
        return None;
    }

    let rest = text.trim().strip_prefix("EVAL[vcp.getWidget{\"")?;
    let (widget, value) = rest.split_once("\"}.setValue{")?;
    let value = value.strip_suffix("}]")?;
    let parameter_number = match widget {
        "x_probed_width" => 1001,
        "x_center_probed" => 1002,
        "y_probed_width" => 1003,
        "y_center_probed" => 1004,
        "x_minus_probed_position" => 1005,
        "x_plus_probed_position" => 1006,
        "y_minus_probed_position" => 1007,
        "y_plus_probed_position" => 1008,
        "z_minus_probed_position" => 1009,
        "averaged_diam" => 1010,
        "edge_delta" => 1011,
        "edge_angle" => 1012,
        "calibration_offset_3032" => 3032,
        _ => return None,
    };
    let value = value.parse::<f64>().ok()?;
    value.is_finite().then_some((parameter_number, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(widget: &str, value: &str) -> String {
        format!("EVAL[vcp.getWidget{{\"{widget}\"}}.setValue{{{value}}}]")
    }

    #[test]
    fn maps_every_widget_for_both_display_kinds() {
        let cases = [
            ("x_probed_width", 1001),
            ("x_center_probed", 1002),
            ("y_probed_width", 1003),
            ("y_center_probed", 1004),
            ("x_minus_probed_position", 1005),
            ("x_plus_probed_position", 1006),
            ("y_minus_probed_position", 1007),
            ("y_plus_probed_position", 1008),
            ("z_minus_probed_position", 1009),
            ("averaged_diam", 1010),
            ("edge_delta", 1011),
            ("edge_angle", 1012),
            ("calibration_offset_3032", 3032),
        ];

        for kind in [MessageKind::OperatorDisplay, MessageKind::NmlDisplay] {
            for (widget, parameter_number) in cases {
                assert_eq!(
                    parse_probe_message(kind, &display(widget, "12.345")),
                    Some((parameter_number, 12.345)),
                    "{kind:?}, widget {widget}"
                );
            }
        }
    }

    #[test]
    fn accepts_surrounding_whitespace() {
        for text in [
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{12.345}]",
            " EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{12.345}] ",
            "\t\nEVAL[vcp.getWidget{\"x_probed_width\"}.setValue{12.345}]\r\n",
        ] {
            assert_eq!(
                parse_probe_message(MessageKind::OperatorDisplay, text),
                Some((1001, 12.345))
            );
        }
    }

    #[test]
    fn accepts_negative_and_scientific_numbers() {
        for (text, expected) in [
            ("-12.345", -12.345),
            ("-0.001", -0.001),
            ("1.25e3", 1250.0),
            ("-2.5E-2", -0.025),
            ("+6.25", 6.25),
        ] {
            assert_eq!(
                parse_probe_message(MessageKind::NmlDisplay, &display("edge_delta", text)),
                Some((1011, expected)),
                "value {text}"
            );
        }
    }

    #[test]
    fn rejects_nan_infinity_and_overflow() {
        for value in [
            "NaN",
            "nan",
            "inf",
            "+inf",
            "-inf",
            "Infinity",
            "-Infinity",
            "1e9999",
            "-1e9999",
        ] {
            assert_eq!(
                parse_probe_message(MessageKind::OperatorDisplay, &display("edge_angle", value)),
                None,
                "value {value}"
            );
        }
    }

    #[test]
    fn ignores_unknown_widgets() {
        for widget in [
            "",
            "X_PROBED_WIDTH",
            "x_probed_width_2",
            "x_probed_width ",
            "spindle_zero_height_3010",
            "coolant_final_angle",
        ] {
            assert_eq!(
                parse_probe_message(MessageKind::NmlDisplay, &display(widget, "1")),
                None,
                "widget {widget:?}"
            );
        }
    }

    #[test]
    fn rejects_non_display_message_kinds() {
        let text = display("x_probed_width", "12.345");
        for kind in [
            MessageKind::OperatorError,
            MessageKind::OperatorText,
            MessageKind::NmlError,
            MessageKind::NmlText,
            MessageKind::Unknown(-1),
            MessageKind::Unknown(3),
            MessageKind::Unknown(13),
        ] {
            assert_eq!(parse_probe_message(kind, &text), None, "{kind:?}");
        }
    }

    #[test]
    fn rejects_wrong_setters() {
        for setter in [
            "setText",
            "SetValue",
            "setvalue",
            "setValueExtra",
            "getValue",
        ] {
            let text = format!("EVAL[vcp.getWidget{{\"x_probed_width\"}}.{setter}{{1}}]");
            assert_eq!(
                parse_probe_message(MessageKind::OperatorDisplay, &text),
                None,
                "setter {setter}"
            );
        }
    }

    #[test]
    fn rejects_malformed_and_trailing_syntax() {
        for text in [
            "",
            "EVAL[]",
            "eval[vcp.getWidget{\"x_probed_width\"}.setValue{1}]",
            "EVAL [vcp.getWidget{\"x_probed_width\"}.setValue{1}]",
            "EVAL[vcp.getWidget {\"x_probed_width\"}.setValue{1}]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue {1}]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{ 1}]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1 }]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{\"1\"}]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}}]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}]]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}] trailing",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}];",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}{2}]",
            "prefix EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}]",
        ] {
            assert_eq!(
                parse_probe_message(MessageKind::NmlDisplay, text),
                None,
                "message {text:?}"
            );
        }
    }

    #[test]
    fn rejects_malformed_numbers() {
        for value in [
            "", "+", "-", ".", "1.2.3", "1e", "1e+", "0x10", "1_000", "--1", "1 2", "1,", "#1001",
            "[1+2]", "1}junk",
        ] {
            assert_eq!(
                parse_probe_message(
                    MessageKind::OperatorDisplay,
                    &display("x_probed_width", value)
                ),
                None,
                "value {value:?}"
            );
        }
    }

    #[test]
    fn rejects_arbitrary_eval_expressions() {
        for text in [
            "EVAL[1 + 1]",
            "EVAL[print(12.345)]",
            "EVAL[__import__('os').system('touch /tmp/probe-message-exploit')]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{__import__('os').system('true')}]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1};__import__('os').system('true')]",
            "EVAL[vcp.getWidget{\"x_probed_width\"}.setValue{1}];__import__('os').system('true')",
        ] {
            for kind in [MessageKind::OperatorDisplay, MessageKind::NmlDisplay] {
                assert_eq!(parse_probe_message(kind, text), None, "{kind:?}: {text}");
            }
        }
    }
}
