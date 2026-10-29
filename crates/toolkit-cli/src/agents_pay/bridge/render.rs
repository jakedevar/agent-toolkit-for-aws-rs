//! Bridge output frames.

use toolkit_core::Json;

const FRAME_CAP: usize = 131_072;
const FAILURE_FRAME: &[u8] = b"{\"ok\":false,\"error\":\"AgentCore request failed\"}\n";

/// Render one complete success frame, replacing an oversized frame atomically.
pub fn frame(value: &Json) -> (Vec<u8>, i32) {
    let mut bytes = crate::aurora::pyfmt::dumps_compact(value).into_bytes();
    bytes.push(b'\n');
    if bytes.len() > FRAME_CAP {
        (FAILURE_FRAME.to_vec(), 1)
    } else {
        (bytes, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::Json;

    // T-BR-OUT-CAP: the encoded document and the final LF share the cap.
    #[test]
    fn t_br_out_cap_ascii_and_escapes() {
        for prefix in ["", "é", "😀"] {
            let encoded_prefix = match prefix {
                "é" => 6,
                "😀" => 12,
                _ => 0,
            };
            for size in [131072, 131073] {
                let value =
                    Json::String(format!("{prefix}{}", "x".repeat(size - encoded_prefix - 3)));
                let (frame, status) = frame(&value);
                if size == 131072 {
                    assert_eq!(frame.len(), size);
                    assert_eq!(frame.last(), Some(&b'\n'));
                    assert_eq!(status, 0);
                } else {
                    assert_eq!(
                        frame,
                        b"{\"ok\":false,\"error\":\"AgentCore request failed\"}\n"
                    );
                    assert_eq!(status, 1);
                }
            }
        }
    }
}
