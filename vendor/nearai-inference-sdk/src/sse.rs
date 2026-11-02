use crate::{
    e2ee::{decrypt_response, invalid_response, ResponseKey},
    InferenceError,
};
use serde_json::Value;

/// Incremental scanner: never rescan an unfinished line and bound each record.
#[derive(Default)]
pub(crate) struct SseDecoder {
    pending: Vec<u8>,
    scan: usize,
    line: usize,
}
const MAX_RECORD_BYTES: usize = 1024 * 1024;
impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<Vec<u8>>, InferenceError> {
        let mut records = Vec::new();
        for part in bytes.chunks(8192) {
            self.pending.extend_from_slice(part);
            self.scan_records(false, &mut records)?;
        }
        if eof {
            self.scan_records(true, &mut records)?;
            if !self.pending.is_empty() {
                records.push(std::mem::take(&mut self.pending));
            }
            self.scan = 0;
            self.line = 0;
        }
        Ok(records)
    }
    fn scan_records(
        &mut self,
        eof: bool,
        records: &mut Vec<Vec<u8>>,
    ) -> Result<(), InferenceError> {
        let mut start = 0;
        while self.scan < self.pending.len() {
            let i = self.scan;
            if i - start >= MAX_RECORD_BYTES {
                return Err(invalid_response().into());
            }
            let byte = self.pending[i];
            if byte != b'\r' && byte != b'\n' {
                self.scan += 1;
                continue;
            }
            if byte == b'\r' && i + 1 == self.pending.len() && !eof {
                break;
            }
            let end = i + if byte == b'\r' && self.pending.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            if end - start > MAX_RECORD_BYTES {
                return Err(invalid_response().into());
            }
            if i == self.line {
                records.push(self.pending[start..end].to_vec());
                start = end;
            }
            self.line = end;
            self.scan = end;
        }
        self.pending.drain(..start);
        self.scan -= start;
        self.line -= start;
        if self.pending.len() > MAX_RECORD_BYTES {
            return Err(invalid_response().into());
        }
        Ok(())
    }
}
fn lines(record: &str) -> Vec<(&str, &str)> {
    let bytes = record.as_bytes();
    let mut result = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\r' && bytes[i] != b'\n' {
            i += 1;
            continue;
        }
        let end = i + if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            2
        } else {
            1
        };
        result.push((&record[start..i], &record[i..end]));
        start = end;
        i = end;
    }
    if start < record.len() {
        result.push((&record[start..], ""));
    }
    result
}
fn data(line: &str) -> Option<&str> {
    if line == "data" {
        Some("")
    } else {
        line.strip_prefix("data:")
            .map(|s| s.strip_prefix(' ').unwrap_or(s))
    }
}
pub(crate) fn transform(
    record: &[u8],
    key: Option<&ResponseKey>,
    id: &mut Option<String>,
) -> Result<Vec<u8>, InferenceError> {
    let text = std::str::from_utf8(record).map_err(|_| invalid_response())?;
    let lines = lines(text);
    let values: Vec<_> = lines.iter().filter_map(|(line, _)| data(line)).collect();
    let joined = values.join("\n");
    if values.is_empty()
        || joined.is_empty()
        || joined == "[DONE]"
        || lines.iter().any(|(s, _)| {
            s.strip_prefix("event:")
                .is_some_and(|e| e.trim() == "error")
        })
    {
        return Ok(record.to_vec());
    }
    let mut body: Value = match serde_json::from_str(&joined) {
        Ok(value) => value,
        Err(_) if key.is_none() => return Ok(record.to_vec()),
        Err(_) => return Err(invalid_response().into()),
    };
    if let Some(value) = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if id.as_ref().is_some_and(|s| s != value) {
            return Err(invalid_response().into());
        }
        *id = Some(value.to_owned());
    }
    let Some(key) = key else {
        return Ok(record.to_vec());
    };
    if !body.is_object() {
        return Err(invalid_response().into());
    }
    if body.get("choices").is_none() {
        return Ok(record.to_vec());
    }
    decrypt_response(&mut body, key, true)?;
    let mut result = String::new();
    let mut remaining = values.len();
    for (line, ending) in lines {
        if data(line).is_none() {
            result.push_str(line);
            result.push_str(ending);
        } else {
            remaining -= 1;
            if remaining == 0 {
                result.push_str("data: ");
                result.push_str(&body.to_string());
                result.push_str(ending);
            }
        }
    }
    Ok(result.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_split_preserves_records_and_utf8() {
        let records = [
            "data: 🌍\r\n\r\n",
            ": heartbeat\r\r",
            "data: [DONE]\n\n",
            ": tail",
        ];
        let input = records.concat().into_bytes();
        for split in 0..=input.len() {
            let mut decoder = SseDecoder::default();
            let mut got = decoder.push(&input[..split], false).unwrap();
            got.extend(decoder.push(&input[split..], true).unwrap());
            assert_eq!(
                got,
                records
                    .iter()
                    .map(|r| r.as_bytes().to_vec())
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn incomplete_records_advance_once_and_are_bounded() {
        let mut decoder = SseDecoder::default();
        for count in 1..=MAX_RECORD_BYTES {
            assert!(decoder.push(b"x", false).unwrap().is_empty());
            assert_eq!(decoder.scan, count);
        }
        assert!(decoder.push(b"x", false).is_err());
        let mut decoder = SseDecoder::default();
        assert!(decoder
            .push(&vec![b'x'; MAX_RECORD_BYTES + 1], true)
            .is_err());
    }
}
