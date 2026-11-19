use super::{sse::{SseDirection, SseTranslator}, sse_frames::Monitor};
use crate::model::Protocol;

fn cases() -> [(Protocol, SseDirection, &'static str, &'static str); 6] {
    use SseDirection::*;
    [
        (Protocol::OpenaiChat, ChatToResponses, "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n", "data: [DONE]\n\n"),
        (Protocol::OpenaiChat, ChatToAnthropic, "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n", "data: [DONE]\n\n"),
        (Protocol::Anthropic, AnthropicToChat, "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n", "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"),
        (Protocol::Anthropic, AnthropicToResponses, "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n", "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"),
        (Protocol::OpenaiResponses, ResponsesToChat, "event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n", "event: response.completed\ndata: {\"response\":{\"status\":\"completed\"}}\n\n"),
        (Protocol::OpenaiResponses, ResponsesToAnthropic, "event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n", "event: response.completed\ndata: {\"response\":{\"status\":\"completed\"}}\n\n"),
    ]
}

#[test]
fn bom_and_carriage_returns_preserve_first_event_in_all_six_directions() {
    for (protocol, direction, partial, terminal) in cases() {
        for separator in ["\n", "\r\n", "\r"] {
            let wire = format!("\u{feff}{partial}{terminal}").replace('\n', separator);
            for width in [1, 2, 7, 1024] {
                let mut translator = SseTranslator::new(direction);
                let mut monitor = Monitor::new(protocol);
                let mut output = String::new();
                for chunk in wire.as_bytes().chunks(width) {
                    output.push_str(&translator.push(chunk));
                    monitor.push(chunk);
                }
                output.push_str(&translator.finish());
                assert!(!translator.failed() && !monitor.failed(true), "{direction:?}: {output}");
                assert!(output.contains("partial"), "{direction:?}: {output}");
            }
        }
    }
}

#[test]
fn incomplete_and_named_error_streams_fail_in_all_six_directions() {
    for (protocol, direction, partial, terminal) in cases() {
        for suffix in ["", "event: error\r\ndata: {\r\ndata: \"message\":\"命名错误\"}\r\n\r\n", "data: {malformed}\n\n", "data: {\"truncated\":", "data: null\n\n"] {
            for width in [1, 7, 1024] {
                let wire = format!("{partial}{suffix}");
                let mut translator = SseTranslator::new(direction);
                let mut monitor = Monitor::new(protocol);
                let mut output = String::new();
                for chunk in wire.as_bytes().chunks(width) {
                    output.push_str(&translator.push(chunk));
                    monitor.push(chunk);
                }
                output.push_str(&translator.finish());
                assert!(translator.failed(), "{direction:?}: {output}");
                assert!(monitor.failed(true));
                assert!(!output.contains("response.completed") && !output.contains("message_stop") && !output.contains("[DONE]"), "{direction:?}: {output}");
                assert!(translator.push(terminal.as_bytes()).is_empty());
                assert!(translator.finish().is_empty());
            }
        }
    }
}

#[test]
fn errors_after_terminal_do_not_flush_success_and_duplicate_terminals_are_idempotent() {
    for (protocol, direction, partial, terminal) in cases() {
        let mut translator = SseTranslator::new(direction);
        let mut output = translator.push(format!("{partial}{terminal}").as_bytes());
        output.push_str(&translator.push(b"event: error\ndata: {\"message\":\"late failure\"}\n\n"));
        output.push_str(&translator.finish());
        assert!(translator.failed());
        assert!(!output.contains("response.completed") && !output.contains("message_stop") && !output.contains("[DONE]"), "{direction:?}: {output}");
        let wire = format!("{partial}{terminal}{terminal}");
        let mut translator = SseTranslator::new(direction);
        let mut monitor = Monitor::new(protocol);
        monitor.push(wire.as_bytes());
        let mut output = translator.push(wire.as_bytes());
        output.push_str(&translator.finish());
        assert!(!translator.failed() && !monitor.failed(true), "{direction:?}: {output}");
        assert!(translator.finish().is_empty());
    }
}
