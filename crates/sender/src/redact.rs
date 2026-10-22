//! API-key redaction. Provider URLs carry the key as a query parameter
//! (`...?api-key=<uuid>`), so any string that might contain an endpoint URL goes through
//! [`redact`] before it reaches a log line, an error message or a file under `data/`.
//! The tracing writers are wrapped in [`RedactingWriter`], so even an unexpected
//! `Display` of a URL cannot leak the key into `run.log.jsonl`.

use std::borrow::Cow;
use std::io::Write;

/// Query parameter names treated as secrets (matched case-insensitively, followed by `=`).
const KEY_PARAMS: [&str; 4] = ["api-key=", "api_key=", "apikey=", "x-api-key="];
pub const REDACTED: &str = "REDACTED";

fn is_value_end(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '&' | '"' | '\'' | '<' | '>' | '#' | ')' | ',' | '\\' | ';'
        )
}

/// Replace the value of every `api-key=` style parameter with `REDACTED`.
pub fn redact(s: &str) -> Cow<'_, str> {
    let lower = s.to_ascii_lowercase();
    if !KEY_PARAMS.iter().any(|p| lower.contains(p)) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let hit = KEY_PARAMS
            .iter()
            .find(|p| lower[i..].starts_with(*p) && s.is_char_boundary(i + p.len()));
        match hit {
            Some(p) => {
                out.push_str(&s[i..i + p.len()]);
                i += p.len();
                let end = s[i..].find(is_value_end).map_or(s.len(), |e| i + e);
                if end > i {
                    out.push_str(REDACTED);
                }
                i = end;
            }
            None => {
                let c = s[i..].chars().next().expect("in bounds");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    Cow::Owned(out)
}

/// True if `s` contains a key parameter with a real-looking value (not `REDACTED`, not a
/// placeholder such as `<key>` or `${HELIUS_KEY}`, at least 8 chars).
pub fn contains_api_key(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    KEY_PARAMS.iter().any(|p| {
        lower.match_indices(p).any(|(at, _)| {
            let v = &s[at + p.len()..];
            let v = &v[..v.find(is_value_end).unwrap_or(v.len())];
            v.len() >= 8
                && v != REDACTED
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
    })
}

/// io::Write adapter that redacts complete lines before passing them on. Formatting
/// machinery may split one line over several `write` calls (and so split a key), so bytes
/// are held until a newline, `flush` or drop.
pub struct RedactingWriter<W: Write> {
    inner: Option<W>,
    pending: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner: Some(inner),
            pending: Vec::new(),
        }
    }

    fn emit(&mut self, upto: usize) -> std::io::Result<()> {
        let chunk: Vec<u8> = self.pending.drain(..upto).collect();
        let text = String::from_utf8_lossy(&chunk);
        match self.inner.as_mut() {
            Some(w) => w.write_all(redact(&text).as_bytes()),
            None => Ok(()),
        }
    }

    pub fn into_inner(mut self) -> W {
        let _ = self.flush();
        self.inner.take().expect("inner writer")
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        if let Some(nl) = self.pending.iter().rposition(|&b| b == b'\n') {
            self.emit(nl + 1)?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            self.emit(self.pending.len())?;
        }
        match self.inner.as_mut() {
            Some(w) => w.flush(),
            None => Ok(()),
        }
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0f3c2b1a-9d8e-4f70-a1b2-c3d4e5f60718";

    #[test]
    fn strips_key_values_and_keeps_the_rest() {
        let url = format!("wss://mainnet.helius-rpc.com/?api-key={KEY}&x=1");
        assert_eq!(
            redact(&url),
            "wss://mainnet.helius-rpc.com/?api-key=REDACTED&x=1"
        );
        let msg = format!("error sending request for url (https://h.example/?API_KEY={KEY})");
        assert_eq!(
            redact(&msg),
            "error sending request for url (https://h.example/?API_KEY=REDACTED)"
        );
        let json = format!("{{\"ws_url\":\"https://a/?apikey={KEY}\"}}");
        assert_eq!(redact(&json), "{\"ws_url\":\"https://a/?apikey=REDACTED\"}");
        assert!(matches!(redact("no keys here"), Cow::Borrowed(_)));
        assert_eq!(redact("ключ api-key=абв"), "ключ api-key=REDACTED");
    }

    #[test]
    fn detector_ignores_placeholders() {
        assert!(contains_api_key(&format!(
            "RPC_URL=https://x/?api-key={KEY}"
        )));
        assert!(!contains_api_key("RPC_URL=https://x/?api-key=REDACTED"));
        assert!(!contains_api_key("RPC_URL=https://x/?api-key=<your-key>"));
        assert!(!contains_api_key(
            "RPC_URL=https://x/?api-key=${HELIUS_API_KEY}"
        ));
        assert!(!contains_api_key("RPC_URL=https://x/?api-key="));
        assert!(!contains_api_key(&redact(&format!("?api-key={KEY}"))));
    }

    #[test]
    fn writer_redacts_log_output() {
        // `write!` splits this into several write calls, one of them mid-URL.
        let mut w = RedactingWriter::new(Vec::new());
        writeln!(w, "{{\"ws_url\":\"wss://h/?api-key={KEY}\"}}").unwrap();
        write!(w, "tail api-key={KEY}").unwrap();
        let out = String::from_utf8(w.into_inner()).unwrap();
        assert!(!out.contains(KEY), "{out}");
        assert!(!contains_api_key(&out));
    }
}
