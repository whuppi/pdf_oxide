//! Content-stream operators from a [`Read`], one at a time.
//!
//! [`Operators`] assembles the lexer's tokens into operands and hands each
//! operator to `build_operator`, so every operator means what it means in
//! `parse_content_stream`. It differs from that parser in five ways:
//!
//! - `1 0 R` is not a reference: content operands are direct objects, so it
//!   reads as two integers followed by the operator `R`.
//! - `true`, `false` and `null` are operands wherever an operand can stand.
//! - A `TJ` array of more than [`TJ_CHUNK`] elements comes out as several
//!   `TJ` operators of at most that many elements each, so no operator holds
//!   more than one chunk of a long array; the elements concatenated are the
//!   original array.
//! - A comment, a NUL or a form feed between an operand and its operator is
//!   white-space; `parse_content_stream` drops the operands there and reads
//!   a comment's words as operators.
//! - The byte after an inline image's `EI` is not read as an operator `I`.
//!
//! Inline images differ too: `parse_content_stream` ends the data at the
//! first white-space, `EI`, white-space-or-delimiter, which cuts a binary
//! image at any ` EI ` inside it. Here the end comes from the dictionary's
//! length, the computed size, or the filter's own end before that rule
//! applies (see [`super::inline`]). The data streams past and the operator
//! carries it only under [`InlineImages::Collect`]. An image that never ends
//! ends the stream and sets [`Damage::inline_unended`]; nothing can be read
//! again from its start, since its data was not kept.
//!
//! [`Operators::damage`] reports everything that was skipped, dropped or
//! cut. [`Operators::pending_names`] and [`consumed_names`] let a consumer
//! tell which names an operator used.
//!
//! An operator keeps its [`MAX_OPERANDS`] newest operands; an older one is
//! dropped and counted ([`Operators::dropped_operands`]), so the window
//! never holds more than that many operands' bytes.
//!
//! Damage is answered as `parse_content_stream` answers it: the operator
//! being read is dropped, its first byte is skipped, and reading goes on
//! from the next byte (see [`Lexer::skip_first_byte`]).

use super::inline::{ImageSpec, Resolver};
use super::lexer::{Lexer, Token, TokenKind};
use crate::cancel::CancelCheck;
use crate::content::operators::Operator;
use crate::content::parser::build_operator;
use crate::error::Result;
use crate::object::Object;
use smallvec::{smallvec, SmallVec};
use std::collections::{HashMap, VecDeque};
use std::io::Read;

/// Most elements one `TJ` operator carries out of a longer array.
pub const TJ_CHUNK: usize = 4096;

/// Most operands any operator takes: `scn` and `SCN` take up to 32 DeviceN
/// components and a pattern name. More pending operands than this belong to
/// no operator.
pub const MAX_OPERANDS: usize = 33;

/// Operators between two polls of the cancel check.
const CANCEL_EVERY: u32 = 4096;

/// Which operators to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every operator.
    Full,
}

/// What an inline image operator carries as its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineImages {
    /// Nothing: `data` is empty and the bytes are never held.
    Skip,
    /// The image's bytes, held for the one operator (its own size).
    Collect,
}

/// Everything the source lost: bytes skipped, operands dropped, text cut.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    /// Bytes skipped to read on after a malformed operator.
    pub skipped: u64,
    /// Operands dropped because an operator held more than [`MAX_OPERANDS`].
    pub dropped_operands: u64,
    /// Operators that followed a long array already sent out as `TJ` chunks.
    pub tj_misreads: u64,
    /// Where the source was given up on after 1024 skipped bytes in a row.
    pub gave_up_at: Option<u64>,
    /// The first string the source ended inside of.
    pub unterminated: Option<TokenKind>,
    /// An inline image reached the end of the source without its end.
    pub inline_unended: bool,
    /// The error the reader returned, which ended the iterator.
    pub read_error: Option<String>,
    /// Parts of a page's `/Contents` that could not be decoded and were skipped.
    pub parts_unreadable: u64,
}

impl Damage {
    /// True when nothing was lost.
    pub fn is_clean(&self) -> bool {
        *self == Self::default()
    }
}

/// An array or dictionary being assembled.
enum Frame {
    Array(Vec<Object>),
    Dict { entries: HashMap<String, Object>, key: Option<String> },
}

/// Iterator over the operators of a content stream; see the module docs.
pub struct Operators<R: Read> {
    lexer: Lexer<R>,
    mode: Mode,
    stack: Vec<Frame>,
    pending: SmallVec<[Object; 6]>,
    /// Absolute offset of the first byte of each pending operand.
    starts: VecDeque<u64>,
    /// Absolute offset of the first byte of the operand being read.
    operand_start: u64,
    in_inline_dict: bool,
    /// An operator is being assembled and the lexer holds its first token.
    active: bool,
    /// A `TJ` chunk of the current operator's array already went out.
    chunked: bool,
    dropped_operands: u64,
    tj_misreads: u64,
    images: InlineImages,
    resolver: Option<Resolver>,
    inline_unended: bool,
    read_error: Option<String>,
    /// Names lexed since the previous operator was handed out.
    names: Vec<String>,
    /// `names.len()` when the operator being assembled began.
    names_mark: usize,
    cancel: Option<CancelCheck>,
    since_check: u32,
    done: bool,
    /// Counts the parts of a page's `/Contents` the source skipped.
    parts_unreadable: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl<R: Read> Operators<R> {
    /// Operators of `reader` in `mode`.
    pub fn new(reader: R, mode: Mode) -> Self {
        Self::from_lexer(Lexer::new(reader), mode)
    }

    /// Operators over a lexer the caller built (a different window size).
    pub fn from_lexer(lexer: Lexer<R>, mode: Mode) -> Self {
        Self {
            lexer,
            mode,
            stack: Vec::new(),
            pending: SmallVec::new(),
            starts: VecDeque::new(),
            operand_start: 0,
            in_inline_dict: false,
            active: false,
            chunked: false,
            dropped_operands: 0,
            tj_misreads: 0,
            images: InlineImages::Collect,
            resolver: None,
            inline_unended: false,
            read_error: None,
            names: Vec::new(),
            names_mark: 0,
            cancel: None,
            since_check: 0,
            done: false,
            parts_unreadable: None,
        }
    }

    /// Reports `counter` as [`Damage::parts_unreadable`]: the source adds to
    /// it for each part it skips.
    pub fn with_parts_counter(mut self, counter: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
        self.parts_unreadable = Some(counter);
        self
    }

    /// Polls `cancel` every 4,096 operators.
    pub fn with_cancel(mut self, cancel: CancelCheck) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// What inline image operators carry as data; `Collect` by default.
    pub fn with_inline_images(mut self, images: InlineImages) -> Self {
        self.images = images;
        self
    }

    /// Resolves a named colour space of an inline image to its component
    /// count, which an unfiltered image's size depends on.
    pub fn with_resolver(mut self, resolver: Resolver) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Every name lexed for the operator just handed out, in order,
    /// operands and nested ones alike. The next call to `next` clears it.
    pub fn pending_names(&self) -> &[String] {
        &self.names
    }

    /// What the source lost so far; final once the iterator has ended.
    pub fn damage(&self) -> Damage {
        Damage {
            skipped: self.lexer.skipped(),
            dropped_operands: self.dropped_operands,
            tj_misreads: self.tj_misreads,
            gave_up_at: self.lexer.gave_up_at(),
            unterminated: self.lexer.unterminated(),
            inline_unended: self.inline_unended,
            read_error: self.read_error.clone(),
            parts_unreadable: self
                .parts_unreadable
                .as_ref()
                .map_or(0, |c| c.load(std::sync::atomic::Ordering::Relaxed)),
        }
    }

    /// The mode the operators were asked for.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The lexer, for its window size and damage counters.
    pub fn lexer(&self) -> &Lexer<R> {
        &self.lexer
    }

    /// Operands dropped because an operator held more than [`MAX_OPERANDS`].
    pub fn dropped_operands(&self) -> u64 {
        self.dropped_operands
    }

    /// Operators that followed an array already sent out as `TJ` chunks but
    /// were not `TJ`: those chunks were wrong. Only `TJ` and `d` take an
    /// array, and a dash array is short, so each one is damage.
    pub fn tj_misreads(&self) -> u64 {
        self.tj_misreads
    }

    fn advance(&mut self) -> Result<Option<Operator>> {
        self.names.clear();
        loop {
            let token = match self.lexer.next_token() {
                Ok(Some(token)) => token,
                Ok(None) => {
                    if self.active {
                        self.fail();
                        continue;
                    }
                    return Ok(None);
                },
                Err(e) => {
                    self.read_error = Some(e.to_string());
                    return Ok(None);
                },
            };
            if self.stack.is_empty() {
                self.operand_start = self.lexer.last_token_offset();
            }
            if !self.active {
                self.active = true;
                self.names_mark = self.names.len();
                self.lexer.anchor_last_token();
            }
            if let Some(op) = self.step(token)? {
                return Ok(Some(op));
            }
        }
    }

    fn step(&mut self, token: Token) -> Result<Option<Operator>> {
        match token {
            Token::Value(object) => self.push(object),
            Token::ArrayStart => {
                self.stack.push(Frame::Array(Vec::new()));
                Ok(None)
            },
            Token::DictStart => {
                self.stack.push(Frame::Dict { entries: HashMap::new(), key: None });
                Ok(None)
            },
            Token::ArrayEnd => match self.stack.pop() {
                Some(Frame::Array(items)) => self.push(Object::Array(items)),
                _ => self.damaged(),
            },
            Token::DictEnd => match self.stack.pop() {
                Some(Frame::Dict { entries, key: None }) => self.push(Object::Dictionary(entries)),
                _ => self.damaged(),
            },
            Token::Word(word) => self.word(word),
            Token::Bad => self.damaged(),
        }
    }

    /// Adds an operand: to the open array or dictionary, else to the operator.
    fn push(&mut self, object: Object) -> Result<Option<Operator>> {
        if let Object::Name(name) = &object {
            self.names.push(name.clone());
        }
        // A lone array operand that reaches a chunk is taken to be a TJ array.
        let lone_array = self.stack.len() == 1 && self.pending.is_empty() && !self.in_inline_dict;
        match self.stack.last_mut() {
            None => {
                if self.pending.len() == MAX_OPERANDS {
                    if self.in_inline_dict {
                        return self.damaged();
                    }
                    self.pending.remove(0);
                    self.starts.pop_front();
                    self.dropped_operands += 1;
                    if let Some(&oldest) = self.starts.front() {
                        self.lexer.anchor_at(oldest);
                    }
                }
                self.pending.push(object);
                self.starts.push_back(self.operand_start);
                debug_assert!(self.pending.len() <= MAX_OPERANDS);
            },
            Some(Frame::Dict { entries, key }) => match key.take() {
                Some(name) => {
                    entries.insert(name, object);
                },
                None => match object {
                    Object::Name(name) => *key = Some(name),
                    _ => return self.damaged(),
                },
            },
            Some(Frame::Array(items)) => {
                if lone_array && items.len() == TJ_CHUNK {
                    let chunk = std::mem::take(items);
                    items.push(object);
                    self.lexer.anchor_here();
                    self.chunked = true;
                    return self.emit(build_operator("TJ", smallvec![Object::Array(chunk)]), false);
                }
                items.push(object);
            },
        }
        Ok(None)
    }

    fn word(&mut self, word: String) -> Result<Option<Operator>> {
        match word.as_str() {
            "true" => return self.push(Object::Boolean(true)),
            "false" => return self.push(Object::Boolean(false)),
            "null" => return self.push(Object::Null),
            _ => {},
        }
        if let Some(top) = self.stack.last() {
            // A bare word as a dictionary value reads as a name.
            if matches!(top, Frame::Dict { key: Some(_), .. }) {
                return self.push(Object::Name(word));
            }
            return self.damaged();
        }
        if self.in_inline_dict {
            if word == "ID" && self.lexer.next_is_space() {
                return self.inline_image();
            }
            return self.damaged();
        }
        if word == "BI" {
            self.pending.clear();
            self.starts.clear();
            self.in_inline_dict = true;
            return Ok(None);
        }
        if self.chunked && word != "TJ" {
            self.tj_misreads += 1;
        }
        self.starts.clear();
        let operands = std::mem::take(&mut self.pending);
        self.emit(build_operator(&word, operands), true)
    }

    /// The operator after `BI <dictionary> ID`: reads the data up to `EI`.
    fn inline_image(&mut self) -> Result<Option<Operator>> {
        self.in_inline_dict = false;
        self.starts.clear();
        let entries = std::mem::take(&mut self.pending);
        if entries.len() % 2 == 1 {
            return self.damaged();
        }
        let mut dict = HashMap::new();
        let mut entries = entries.into_iter();
        while let (Some(key), Some(value)) = (entries.next(), entries.next()) {
            if let Object::Name(name) = key {
                dict.insert(name, value);
            }
        }
        let spec = ImageSpec::from_dict(&dict, self.resolver.as_ref());
        // The data streams past from here, so the operator cannot be read
        // again from its first byte.
        self.lexer.release_anchor();
        self.active = false;
        self.lexer.skip_one_space();
        let collect = self.images == InlineImages::Collect;
        match self.lexer.read_inline_data(&spec, collect) {
            Ok(Some(data)) => self.emit(Operator::InlineImage { dict: Box::new(dict), data }, true),
            Ok(None) => {
                self.inline_unended = true;
                Ok(None)
            },
            Err(e) => {
                self.read_error = Some(e.to_string());
                Ok(None)
            },
        }
    }

    /// Hands out `op`; `ends_operator` is false for a `TJ` chunk, after
    /// which the same array continues.
    fn emit(&mut self, op: Operator, ends_operator: bool) -> Result<Option<Operator>> {
        self.lexer.note_operator();
        if ends_operator {
            self.active = false;
            self.chunked = false;
            self.lexer.release_anchor();
        }
        self.since_check += 1;
        if self.since_check >= CANCEL_EVERY {
            self.since_check = 0;
            if let Some(cancel) = &self.cancel {
                cancel.check()?;
            }
        }
        Ok(Some(op))
    }

    fn damaged(&mut self) -> Result<Option<Operator>> {
        self.fail();
        Ok(None)
    }

    /// Drops the operator being assembled and reads again from the byte
    /// after its first.
    fn fail(&mut self) {
        self.stack.clear();
        self.pending.clear();
        self.starts.clear();
        self.in_inline_dict = false;
        self.active = false;
        self.chunked = false;
        self.names.truncate(self.names_mark);
        self.lexer.skip_first_byte();
    }
}

/// The names `op` looked up in the page's resources, one arm per operator:
/// `Tf`, `Do`, `cs`, `CS`, `gs` and `sh` their name operand; `scn` and `SCN`
/// their last operand when it is a name; `BDC` and `DP` their property list
/// when that is a name, never the tag; an inline image the names in its `/CS` value.
/// `DP` (an `Other`, since it has no variant) reads like `BDC`. `BMC`, `MP`,
/// `d0`, `d1` and every other `Other` use none.
///
/// The names the operator was built from are the ones `build_operator`
/// kept: a `Do` with stray operands before it uses its last.
pub fn consumed_names(op: &Operator) -> SmallVec<[&str; 2]> {
    fn add<'a>(names: &mut SmallVec<[&'a str; 2]>, name: &'a str) {
        if !name.is_empty() {
            names.push(name);
        }
    }
    let mut names: SmallVec<[&str; 2]> = SmallVec::new();
    match op {
        Operator::Tf { font: name, .. }
        | Operator::Do { name }
        | Operator::SetFillColorSpace { name }
        | Operator::SetStrokeColorSpace { name }
        | Operator::PaintShading { name }
        | Operator::SetExtGState { dict_name: name } => add(&mut names, name),
        Operator::SetFillColorN { name: Some(name), .. }
        | Operator::SetStrokeColorN { name: Some(name), .. } => add(&mut names, name),
        Operator::BeginMarkedContentDict { properties, .. } => {
            if let Object::Name(name) = &**properties {
                add(&mut names, name);
            }
        },
        // `DP` has no operator of its own; it reads like `BDC`.
        Operator::Other { name, operands } if name == "DP" => {
            if let Some(Object::Name(properties)) = operands.get(1) {
                add(&mut names, properties);
            }
        },
        Operator::InlineImage { dict, .. } => {
            match dict.get("CS").or_else(|| dict.get("ColorSpace")) {
                Some(Object::Name(name)) => add(&mut names, name),
                Some(Object::Array(items)) => {
                    for item in items {
                        if let Object::Name(name) = item {
                            add(&mut names, name);
                        }
                    }
                },
                _ => {},
            }
        },
        _ => {},
    }
    names
}

impl<R: Read> Iterator for Operators<R> {
    type Item = Result<Operator>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.advance() {
            Ok(Some(op)) => Some(Ok(op)),
            Ok(None) => {
                self.done = true;
                None
            },
            Err(e) => {
                self.done = true;
                Some(Err(e))
            },
        }
    }
}
