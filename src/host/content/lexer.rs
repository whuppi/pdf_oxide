//! Streaming tokenizer for content streams.
//!
//! The lexer reads a [`Read`] into a window (64 KiB to start) and yields one
//! complete token at a time. A token that reaches the end of the window
//! before its terminator, while the source has more, makes the window
//! compact, refill and rescan that token from its first byte; the window
//! doubles only as far as that one token needs. One token is the unit of
//! memory, so a single string literal is held whole, and a string or hex
//! string that never closes is held to the end of the source, because only
//! the end proves it never closes.
//!
//! A token's value comes from `parser::parse_object` run on the token's own
//! bytes, the function `parse_content_stream` uses, so numbers, names (with
//! `#xx` escapes) and strings mean exactly what they mean there.
//!
//! A byte no token can start with, and a token whose value does not parse,
//! come out as [`Token::Bad`]. `parse_content_stream` answers any error by
//! skipping the first byte of the operator it was reading and reading again
//! from the next byte, and giving up after 1024 consecutive errors. The
//! caller anchors the first token of an operator; the window keeps every
//! byte from the anchor on, and [`Lexer::skip_first_byte`] does the same
//! skip, so the bytes of a dropped operand are read again as new tokens.
//! The window therefore holds one operator's tokens, and a chunk of a long
//! `TJ` array, at most.

use crate::object::Object;
use crate::parser::parse_object;
use std::io::{self, Read};

/// Starting size of the window, in bytes.
pub const WINDOW: usize = 64 * 1024;

/// Consecutive skipped bytes after which the lexer gives up.
const MAX_CONSECUTIVE_SKIPS: u32 = 1024;

/// One token of a content stream.
#[derive(Debug)]
pub enum Token {
    /// A number, name, literal string or hex string, as `parse_object` reads it.
    Value(Object),
    /// `[`
    ArrayStart,
    /// `]`
    ArrayEnd,
    /// `<<`
    DictStart,
    /// `>>`
    DictEnd,
    /// A run of letters, digits, `'`, `"` and `*` starting with a letter, `'`, `"` or `*`:
    /// an operator, or `true`, `false`, `null`, `BI`, `ID`.
    Word(String),
    /// A byte or token the parser rejects.
    Bad,
}

/// PDF white-space (ISO 32000-1 7.2.2, Table 1).
pub(crate) fn is_space(b: u8) -> bool {
    matches!(b, 0x00 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// PDF delimiter (ISO 32000-1 7.2.2, Table 2).
pub(crate) fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

fn is_regular(b: u8) -> bool {
    !is_space(b) && !is_delim(b)
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'\'' | b'"' | b'*')
}

#[derive(Clone, Copy)]
enum Kind {
    Value,
    Word,
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    Bad,
}

enum Scan {
    Token(usize, Kind),
    /// The token reaches the window's end and the source has more.
    More,
}

/// Where the first token in `b` ends; `b` is not empty and starts neither
/// with white-space nor with a comment.
fn scan(b: &[u8], eof: bool) -> Scan {
    let len = b.len();
    let ends_here = |n: usize| n == len && !eof;
    match b[0] {
        b'/' => {
            let n = 1 + b[1..].iter().take_while(|&&c| is_regular(c)).count();
            if ends_here(n) {
                Scan::More
            } else {
                Scan::Token(n, Kind::Value)
            }
        },
        b'0'..=b'9' | b'+' | b'-' | b'.' => {
            let digits = |from: usize| from + b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
            let mut i = usize::from(matches!(b[0], b'+' | b'-'));
            i = digits(i);
            if i < len && b[i] == b'.' {
                i = digits(i + 1);
            }
            if ends_here(i) {
                Scan::More
            } else {
                Scan::Token(i, Kind::Value)
            }
        },
        b'(' => {
            let mut depth = 1u32;
            let mut i = 1;
            while i < len {
                match b[i] {
                    b'\\' => {
                        if i + 1 >= len {
                            break;
                        }
                        i += 2;
                        continue;
                    },
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Scan::Token(i + 1, Kind::Value);
                        }
                    },
                    _ => {},
                }
                i += 1;
            }
            if eof {
                Scan::Token(1, Kind::Bad)
            } else {
                Scan::More
            }
        },
        b'<' => {
            if len < 2 {
                return if eof { Scan::Token(1, Kind::Bad) } else { Scan::More };
            }
            if b[1] == b'<' {
                return Scan::Token(2, Kind::DictStart);
            }
            match b[1..].iter().position(|&c| c == b'>') {
                Some(p) => Scan::Token(p + 2, Kind::Value),
                None if eof => Scan::Token(1, Kind::Bad),
                None => Scan::More,
            }
        },
        b'>' => {
            if len < 2 {
                return if eof { Scan::Token(1, Kind::Bad) } else { Scan::More };
            }
            if b[1] == b'>' {
                Scan::Token(2, Kind::DictEnd)
            } else {
                Scan::Token(1, Kind::Bad)
            }
        },
        b'[' => Scan::Token(1, Kind::ArrayStart),
        b']' => Scan::Token(1, Kind::ArrayEnd),
        c if c.is_ascii_alphabetic() || matches!(c, b'\'' | b'"' | b'*') => {
            let n = b.iter().take_while(|&&c| is_word_byte(c)).count();
            if ends_here(n) {
                Scan::More
            } else {
                Scan::Token(n, Kind::Word)
            }
        },
        _ => Scan::Token(1, Kind::Bad),
    }
}

/// The kind of token a source ended inside of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// A `(` string with no closing `)`.
    LiteralString,
    /// A `<` string with no closing `>`.
    HexString,
}

/// Streaming tokenizer; see the module docs.
pub struct Lexer<R: Read> {
    reader: R,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    in_comment: bool,
    /// Absolute offset of `buf[start]`.
    offset: u64,
    /// Index in `buf` of the first byte of the token read last.
    token_idx: usize,
    /// Index in `buf` and absolute offset of the anchored operator's first byte.
    anchor: Option<(usize, u64)>,
    skipped: u64,
    run: u32,
    gave_up_at: Option<u64>,
    unterminated: Option<TokenKind>,
}

impl<R: Read> Lexer<R> {
    /// A lexer over `reader` with a window of [`WINDOW`] bytes.
    pub fn new(reader: R) -> Self {
        Self::with_window(reader, WINDOW)
    }

    /// A lexer with a window of `window` bytes to start (at least 8).
    pub fn with_window(reader: R, window: usize) -> Self {
        Self {
            reader,
            buf: vec![0; window.max(8)],
            start: 0,
            end: 0,
            eof: false,
            in_comment: false,
            offset: 0,
            token_idx: 0,
            anchor: None,
            skipped: 0,
            run: 0,
            gave_up_at: None,
            unterminated: None,
        }
    }

    /// Bytes the window can hold now.
    pub fn window_capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Absolute offset of the next unread byte.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Bytes skipped as damage so far.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// The offset where the lexer gave up, once 1024 consecutive bytes were skipped.
    pub fn gave_up_at(&self) -> Option<u64> {
        self.gave_up_at
    }

    /// Anchors the token read last as the first of an operator: the window
    /// keeps its bytes and every byte after it until [`Self::release_anchor`].
    pub fn anchor_last_token(&mut self) {
        self.anchor = Some((self.token_idx, self.last_token_offset()));
    }

    /// Absolute offset of the first byte of the token read last.
    pub fn last_token_offset(&self) -> u64 {
        self.offset - (self.start - self.token_idx) as u64
    }

    /// Moves the anchor forward to the absolute offset `at`, which lies
    /// between the anchor and the next unread byte: the window drops the
    /// bytes before it at the next refill.
    pub fn anchor_at(&mut self, at: u64) {
        let back = (self.offset - at) as usize;
        self.anchor = Some((self.start - back, at));
    }

    /// Anchors the next unread byte, after a `TJ` chunk went out.
    pub fn anchor_here(&mut self) {
        self.anchor = Some((self.start, self.offset));
    }

    /// The anchored operator was read whole.
    pub fn release_anchor(&mut self) {
        self.anchor = None;
    }

    /// The anchored operator is damaged: counts one skipped byte and reads
    /// again from the byte after the anchor. After 1024 in a row the lexer
    /// gives up at the anchor instead.
    pub fn skip_first_byte(&mut self) {
        self.skipped += 1;
        self.run += 1;
        let (idx, at) = self.anchor.take().unwrap_or((self.start, self.offset));
        if self.run >= MAX_CONSECUTIVE_SKIPS {
            self.gave_up_at = Some(at);
            return;
        }
        self.start = idx + 1;
        self.offset = at + 1;
        self.in_comment = false;
    }

    /// An operator was read whole: the run of skipped bytes ends.
    pub fn note_operator(&mut self) {
        self.run = 0;
    }

    /// True when the next byte is white-space or there is none.
    pub(crate) fn next_is_space(&self) -> bool {
        self.start == self.end || is_space(self.buf[self.start])
    }

    fn consume(&mut self, n: usize) {
        self.start += n;
        self.offset += n as u64;
    }

    /// Reads once, after compacting and, when the window is full, doubling it.
    fn fill(&mut self) -> io::Result<()> {
        if self.eof {
            return Ok(());
        }
        let keep = self.anchor.map_or(self.start, |(idx, _)| idx.min(self.start));
        if keep > 0 {
            self.buf.copy_within(keep..self.end, 0);
            self.end -= keep;
            self.start -= keep;
            if let Some((idx, _)) = &mut self.anchor {
                *idx -= keep;
            }
        }
        if self.end == self.buf.len() {
            let doubled = self.buf.len() * 2;
            self.buf.resize(doubled, 0);
        }
        loop {
            match self.reader.read(&mut self.buf[self.end..]) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                },
                Ok(n) => {
                    self.end += n;
                    return Ok(());
                },
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {},
                Err(e) => return Err(e),
            }
        }
    }

    /// The next token, or `None` at the end of the source or after giving up.
    pub fn next_token(&mut self) -> io::Result<Option<Token>> {
        if self.gave_up_at.is_some() {
            return Ok(None);
        }
        loop {
            if self.start == self.end {
                if self.eof {
                    return Ok(None);
                }
                self.fill()?;
                continue;
            }
            if self.in_comment {
                let line_end = self.buf[self.start..self.end]
                    .iter()
                    .position(|&c| c == b'\r' || c == b'\n');
                match line_end {
                    Some(p) => {
                        self.in_comment = false;
                        self.consume(p);
                    },
                    None => self.consume(self.end - self.start),
                }
                continue;
            }
            let first = self.buf[self.start];
            if is_space(first) {
                self.consume(1);
                continue;
            }
            if first == b'%' {
                self.in_comment = true;
                self.consume(1);
                continue;
            }
            match scan(&self.buf[self.start..self.end], self.eof) {
                Scan::More => self.fill()?,
                Scan::Token(len, kind) => {
                    let bytes = &self.buf[self.start..self.start + len];
                    let token = match kind {
                        Kind::Value => match parse_object(bytes) {
                            Ok((_, object)) => Token::Value(object),
                            Err(_) => Token::Bad,
                        },
                        Kind::Word => Token::Word(String::from_utf8_lossy(bytes).into_owned()),
                        Kind::ArrayStart => Token::ArrayStart,
                        Kind::ArrayEnd => Token::ArrayEnd,
                        Kind::DictStart => Token::DictStart,
                        Kind::DictEnd => Token::DictEnd,
                        Kind::Bad => {
                            let open = match self.buf[self.start] {
                                b'(' => Some(TokenKind::LiteralString),
                                b'<' => Some(TokenKind::HexString),
                                _ => None,
                            };
                            if self.unterminated.is_none() {
                                self.unterminated = open;
                            }
                            Token::Bad
                        },
                    };
                    self.token_idx = self.start;
                    self.consume(len);
                    return Ok(Some(token));
                },
            }
        }
    }

    /// The kind of token the source ended inside of, if it did.
    pub fn unterminated(&self) -> Option<TokenKind> {
        self.unterminated
    }

    /// The unread bytes in the window.
    pub(super) fn window(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }

    /// True once the reader has returned 0; the window may still hold bytes.
    pub(super) fn at_eof(&self) -> bool {
        self.eof
    }

    /// Drops one white-space byte from the front of the window, if it is one.
    pub(super) fn skip_one_space(&mut self) {
        if self.start < self.end && is_space(self.buf[self.start]) {
            self.consume(1);
        }
    }

    /// Drops `n` bytes from the front of the window.
    pub(super) fn discard(&mut self, n: usize) {
        self.consume(n);
    }

    /// Reads once more into the window, dropping the bytes already discarded.
    pub(super) fn refill(&mut self) -> io::Result<()> {
        self.fill()
    }
}
