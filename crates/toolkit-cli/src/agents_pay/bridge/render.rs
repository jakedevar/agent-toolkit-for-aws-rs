//! Bridge output frames.

#[cfg(test)]
mod tests {
    use toolkit_core::Json;
    use super::*;

    // T-BR-OUT-CAP: the encoded document and the final LF share the cap.
    #[test]
    fn t_br_out_cap_ascii_and_escapes() {
        for prefix in ["", "é", "😀"] {
            let encoded_prefix = match prefix { "é" => 6, "😀" => 12, _ => 0 };
            for size in [131072, 131073] {
                let value = Json::String(format!("{prefix}{}", "x".repeat(size - encoded_prefix - 3)));
                let (frame, status) = frame(&value);
                if size == 131072 {
                    assert_eq!(frame.len(), size);
                    assert_eq!(frame.last(), Some(&b'\n'));
                    assert_eq!(status, 0);
                } else {
                    assert_eq!(frame, b"{\"ok\":false,\"error\":\"AgentCore request failed\"}\n");
                    assert_eq!(status, 1);
                }
            }
        }
    }
}
