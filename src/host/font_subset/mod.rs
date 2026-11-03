//! Embedded font programs cut down to the glyphs the document shows, with
//! every glyph id kept.
//!
//! This file is part of the pdf_manipulator host layer (NOT upstream).
//!
//! A full rewrite that prunes resources asks [`plan`] last. It reads every
//! content of the kept pages one at a time (the page, the forms, patterns,
//! soft-mask groups and Type 3 glyph procedures its resources hold, and the
//! appearance streams of its annotations), as operators, and collects for
//! each font program the glyph ids its fonts show. It answers with the
//! replacement stream of each program it could shrink; the save writes those
//! in place of the source's.
//!
//! Kinds of font that are subset: a simple TrueType font (`/FontFile2`), a
//! CIDFontType2 under `Identity-H`/`Identity-V` (`/FontFile2`) and a
//! CIDFontType0 under the same (`/FontFile3`, `/CIDFontType0C`). A font
//! program is left whole when:
//! - any font that uses it is of another kind, has a CMap other than
//!   `Identity-H`/`Identity-V`, or cannot be read;
//! - a form field can use it: it is in an AcroForm or field `/DR`, a widget's
//!   appearance resources hold it, or a page the save edits holds it;
//! - an `ExtGState` `/Font` names it, or a resource dictionary no walk reached
//!   holds it;
//! - it has a `GSUB` table, since a shaper may then substitute a glyph no
//!   code names;
//! - the program is shared with a merged source's object;
//! - the subset, deflated, is not smaller than the program as the save would
//!   store it without the plan.
//!
//! The whole plan is dropped when a content shows text with no font set (the
//! font is then the painter's), names a font its resources do not hold, or
//! cannot be read to its end.
//!
//! The glyphs a code shows are every glyph any mapping of the code can
//! reach: the CID-to-GID of the font, the font's own `cmap` for the code, for
//! the code in the symbol range, and for each character the code decodes to,
//! and the glyph of a `/Differences` name. That is more than one renderer
//! paints and exactly what any of them may.
//!
//! The memory of a plan is the glyph sets, the fonts met, and one font
//! program at a time: a content is never held.

pub(crate) mod cff;
pub(crate) mod truetype;

use crate::cancel::CancelCheck;
use crate::content::operators::{Operator, TextElement};
use crate::document::PdfDocument;
use crate::error::Result;
use crate::fonts::unicode_decode::TextCharIter;
use crate::fonts::{CIDToGIDMap, FontInfo};
use crate::host::content::inline::components;
use crate::host::content::parts::{decoded_reader, PartsReader};
use crate::host::content::{InlineImages, Mode, Operators};
use crate::object::{Object, ObjectRef};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

type Dict = HashMap<String, Object>;

/// Forms painted from forms deeper than this are not walked, and the plan
/// is dropped.
const MAX_NESTING: usize = 64;

/// A font-map holder no walk reaches.
const NEVER: u32 = u32::MAX;

/// What a plan reads, and how it writes.
pub(crate) struct Scope<'a> {
    pub doc: &'a PdfDocument,
    /// Objects the editor stages over the document's own.
    pub staged: &'a HashMap<u32, Object>,
    /// Objects the save replaces.
    pub overrides: &'a HashMap<u32, Object>,
    /// The duplicate-to-kept map of the save: a program written once for
    /// two streams must hold the glyphs of both.
    pub aliases: &'a HashMap<u32, u32>,
    /// The kept pages, and whether the save edits each while writing.
    pub pages: Vec<(u32, bool)>,
    /// The Flate level of the save.
    pub level: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    TrueTypeSimple,
    TrueTypeCid,
    CffCid,
}

struct Entry {
    info: FontInfo,
    file: u32,
    kind: Kind,
    memo: HashMap<u16, Vec<u16>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ResId {
    Obj(u32),
    Direct(u32),
}

/// A resource dictionary as the walk meets it.
struct Res {
    id: ResId,
    dict: Dict,
    fonts: Dict,
    /// The id of the object that holds the font map itself.
    holder: u32,
    spaces: Arc<HashMap<String, usize>>,
}

/// The font a content has selected.
#[derive(Clone, Copy)]
enum Current {
    Unset,
    Unresolved,
    /// A font no glyphs are collected for.
    Other,
    Font(u32),
}

struct Walker<'a> {
    doc: &'a PdfDocument,
    staged: &'a HashMap<u32, Object>,
    overrides: &'a HashMap<u32, Object>,
    fonts: HashMap<u32, Option<Entry>>,
    kinds: HashMap<u32, Kind>,
    glyphs: HashMap<u32, BTreeSet<u16>>,
    poisoned: HashSet<u32>,
    walked: HashSet<u32>,
    resources: HashMap<ResId, Rc<Res>>,
    visited: HashSet<(u32, ResId, bool)>,
    done: HashSet<(ResId, bool)>,
    disabled: bool,
    cancel: CancelCheck,
}

fn dict_of(obj: &Object) -> Option<&Dict> {
    match obj {
        Object::Dictionary(dict) => Some(dict),
        Object::Stream { dict, .. } => Some(dict),
        _ => None,
    }
}

/// The programs `font`'s descriptors hold, whatever the kind of font.
fn descriptor_files(walker: &Walker<'_>, font: &Dict) -> Vec<u32> {
    let mut out = Vec::new();
    let mut descriptors = Vec::new();
    descriptors.extend(font.get("FontDescriptor").cloned());
    if let Some(list) = font.get("DescendantFonts").and_then(|v| walker.resolve(v)) {
        let items: Vec<Object> = match &list {
            Object::Array(items) => items.clone(),
            other => vec![other.clone()],
        };
        for item in items {
            if let Some(cid) = walker.resolve(&item) {
                descriptors.extend(dict_of(&cid).and_then(|d| d.get("FontDescriptor")).cloned());
            }
        }
    }
    for descriptor in descriptors {
        let Some(descriptor) = walker.resolve(&descriptor) else { continue };
        let Some(dict) = dict_of(&descriptor) else { continue };
        for key in ["FontFile", "FontFile2", "FontFile3"] {
            if let Some(Object::Reference(r)) = dict.get(key) {
                out.push(r.id);
            }
        }
    }
    out
}

impl<'a> Walker<'a> {
    fn read(&self, id: u32) -> Option<Object> {
        match self.overrides.get(&id).or_else(|| self.staged.get(&id)) {
            Some(obj) => Some(obj.clone()),
            None => self.doc.load_object(ObjectRef::new(id, 0)).ok(),
        }
    }

    /// `value` itself, or the object it references.
    fn resolve(&self, value: &Object) -> Option<Object> {
        match value {
            Object::Reference(r) => self.read(r.id),
            other => Some(other.clone()),
        }
    }

    fn dict_value(&self, value: &Object) -> Option<Dict> {
        dict_of(&self.resolve(value)?).cloned()
    }

    fn poison_files(&mut self, files: Vec<u32>) {
        self.poisoned.extend(files);
    }

    fn poison_font(&mut self, id: u32) {
        if let Some(font) = self.read(id) {
            if let Some(dict) = dict_of(&font) {
                let files = descriptor_files(self, dict);
                self.poison_files(files);
            }
        }
    }

    fn poison_font_value(&mut self, value: &Object) {
        match value {
            Object::Reference(r) => self.poison_font(r.id),
            Object::Dictionary(dict) => {
                let files = descriptor_files(self, dict);
                self.poison_files(files);
            },
            _ => {},
        }
    }

    fn poison_map(&mut self, map: &Dict) {
        for value in map.values() {
            self.poison_font_value(value);
        }
    }

    fn resources(&mut self, owner: u32, value: &Object) -> Option<Rc<Res>> {
        let (id, dict, font_owner) = match value {
            Object::Reference(r) => {
                let obj = self.read(r.id)?;
                (ResId::Obj(r.id), dict_of(&obj)?.clone(), r.id)
            },
            Object::Dictionary(dict) => (ResId::Direct(owner), dict.clone(), owner),
            _ => return None,
        };
        if let Some(hit) = self.resources.get(&id) {
            return Some(hit.clone());
        }
        let (fonts, holder) = match dict.get("Font") {
            Some(Object::Reference(r)) => match self.dict_value(&Object::Reference(*r)) {
                Some(map) => (map, r.id),
                None => (Dict::new(), NEVER),
            },
            Some(Object::Dictionary(map)) => (map.clone(), font_owner),
            _ => (Dict::new(), NEVER),
        };
        let mut spaces = HashMap::new();
        if let Some(map) = dict.get("ColorSpace").and_then(|v| self.dict_value(v)) {
            for (name, value) in &map {
                if let Some(space) = self.resolve(value) {
                    if let Some(n) = components(&space, None) {
                        spaces.insert(name.clone(), n);
                    }
                }
            }
        }
        let res = Rc::new(Res { id, dict, fonts, holder, spaces: Arc::new(spaces) });
        self.resources.insert(id, res.clone());
        Some(res)
    }

    /// The resources a page uses: its own, or the nearest ancestor's.
    fn page_resources(&mut self, page: u32) -> Option<Rc<Res>> {
        let mut at = page;
        for _ in 0..MAX_NESTING {
            let obj = self.read(at)?;
            let dict = dict_of(&obj)?;
            if let Some(value) = dict.get("Resources") {
                return self.resources(at, value);
            }
            at = dict.get("Parent")?.as_reference()?.id;
        }
        None
    }

    fn classify(&self, font: &Dict) -> Option<(Kind, u32)> {
        let file = |descriptor: &Object, key: &str| -> Option<u32> {
            let descriptor = self.resolve(descriptor)?;
            let dict = dict_of(&descriptor)?;
            let others = ["FontFile", "FontFile2", "FontFile3"].iter().filter(|k| **k != key);
            if others.into_iter().any(|k| dict.contains_key(*k)) {
                return None;
            }
            Some(dict.get(key)?.as_reference()?.id)
        };
        match font.get("Subtype")?.as_name()? {
            "TrueType" => {
                Some((Kind::TrueTypeSimple, file(font.get("FontDescriptor")?, "FontFile2")?))
            },
            "Type0" => {
                if !matches!(font.get("Encoding")?.as_name()?, "Identity-H" | "Identity-V") {
                    return None;
                }
                let descendants = self.resolve(font.get("DescendantFonts")?)?;
                let first = match &descendants {
                    Object::Array(items) => items.first()?.clone(),
                    other => other.clone(),
                };
                let cid = self.dict_value(&first)?;
                let descriptor = cid.get("FontDescriptor")?;
                match cid.get("Subtype")?.as_name()? {
                    "CIDFontType2" => Some((Kind::TrueTypeCid, file(descriptor, "FontFile2")?)),
                    "CIDFontType0" => {
                        let id = file(descriptor, "FontFile3")?;
                        let program = self.read(id)?;
                        let subtype = dict_of(&program)?.get("Subtype")?.as_name()?;
                        (subtype == "CIDFontType0C").then_some((Kind::CffCid, id))
                    },
                    _ => None,
                }
            },
            _ => None,
        }
    }

    /// Registers font `id` the first time it is met: a font that cannot be
    /// subset poisons its programs.
    fn font(&mut self, id: u32) {
        if self.fonts.contains_key(&id) {
            return;
        }
        let entry = (|| {
            let obj = self.read(id)?;
            let dict = dict_of(&obj)?;
            let (kind, file) = self.classify(dict)?;
            let info = FontInfo::from_dict(&Object::Dictionary(dict.clone()), self.doc).ok()?;
            info.embedded_font_data.as_ref()?;
            Some(Entry { info, file, kind, memo: HashMap::new() })
        })();
        match &entry {
            Some(entry) => {
                if *self.kinds.entry(entry.file).or_insert(entry.kind) != entry.kind {
                    self.poisoned.insert(entry.file);
                }
            },
            None => self.poison_font(id),
        }
        self.fonts.insert(id, entry);
    }

    fn show(&mut self, font: u32, bytes: &[u8]) {
        let Some(Some(entry)) = self.fonts.get_mut(&font) else { return };
        let set = self.glyphs.entry(entry.file).or_default();
        match entry.kind {
            Kind::TrueTypeSimple => {
                for &code in bytes {
                    set.extend(entry.reach(code as u16));
                }
            },
            Kind::TrueTypeCid | Kind::CffCid => {
                let codes: Vec<u16> =
                    TextCharIter::new(bytes, Some(&entry.info)).map(|(code, _)| code).collect();
                for code in codes {
                    set.extend(entry.reach(code));
                }
            },
        }
    }

    fn operators(
        &self,
        reader: Box<dyn Read>,
        res: &Res,
        parts: Option<Arc<AtomicU64>>,
    ) -> Operators<Box<dyn Read>> {
        let mut ops = Operators::new(reader, Mode::Full);
        if let Some(parts) = parts {
            ops = ops.with_parts_counter(parts);
        }
        let spaces = res.spaces.clone();
        ops.with_cancel(self.cancel.clone())
            .with_inline_images(InlineImages::Skip)
            .with_resolver(Box::new(move |name| spaces.get(name).copied()))
    }

    /// Reads one content against `res`, collecting what it shows.
    fn run(&mut self, mut ops: Operators<Box<dyn Read>>, res: &Res, poison: bool) -> Result<()> {
        let mut current = Current::Unset;
        let mut saved: Vec<Current> = Vec::new();
        while let Some(op) = ops.next() {
            match op? {
                Operator::SaveState => saved.push(current),
                Operator::RestoreState => {
                    if let Some(previous) = saved.pop() {
                        current = previous;
                    }
                },
                Operator::Tf { font, .. } => {
                    current = match res.fonts.get(&font) {
                        Some(Object::Reference(r)) => {
                            if poison {
                                self.poison_font(r.id);
                            }
                            self.font(r.id);
                            Current::Font(r.id)
                        },
                        Some(direct) => {
                            self.poison_font_value(direct);
                            Current::Other
                        },
                        None => Current::Unresolved,
                    };
                },
                Operator::Tj { text } | Operator::Quote { text } => self.shown(current, &text),
                Operator::DoubleQuote { text, .. } => self.shown(current, &text),
                Operator::TJ { array } => {
                    for item in array {
                        if let TextElement::String(text) = item {
                            self.shown(current, &text);
                        }
                    }
                },
                _ => {},
            }
        }
        if !ops.damage().is_clean() {
            self.disabled = true;
        }
        Ok(())
    }

    fn shown(&mut self, current: Current, text: &[u8]) {
        match current {
            Current::Font(id) => self.show(id, text),
            Current::Other => {},
            Current::Unset | Current::Unresolved => self.disabled = true,
        }
    }

    /// Walks everything `res` holds that has content of its own.
    fn context(&mut self, res: &Rc<Res>, poison: bool, depth: usize) -> Result<()> {
        if res.holder != NEVER {
            self.walked.insert(res.holder);
        }
        if poison {
            let map = res.fonts.clone();
            self.poison_map(&map);
        }
        if !self.done.insert((res.id, poison)) {
            return Ok(());
        }
        let sorted = |map: Dict| -> Vec<Object> {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            keys.into_iter().filter_map(|k| map.get(&k).cloned()).collect()
        };
        if let Some(map) = res.dict.get("XObject").and_then(|v| self.dict_value(v)) {
            for value in sorted(map) {
                let Object::Reference(r) = value else { continue };
                let Some(Object::Stream { dict, .. }) = self.read(r.id) else { continue };
                if !matches!(dict.get("Subtype").and_then(|s| s.as_name()), Some("Image" | "PS")) {
                    self.stream(r.id, res, poison, depth + 1)?;
                }
            }
        }
        if let Some(map) = res.dict.get("Pattern").and_then(|v| self.dict_value(v)) {
            for value in sorted(map) {
                let Object::Reference(r) = value else { continue };
                if matches!(self.read(r.id), Some(Object::Stream { .. })) {
                    self.stream(r.id, res, poison, depth + 1)?;
                }
            }
        }
        if let Some(map) = res.dict.get("ExtGState").and_then(|v| self.dict_value(v)) {
            for value in sorted(map) {
                let Some(state) = self.dict_value(&value) else { continue };
                let Some(mask) = state.get("SMask").and_then(|v| self.dict_value(v)) else {
                    continue;
                };
                if let Some(Object::Reference(group)) = mask.get("G") {
                    self.stream(group.id, res, poison, depth + 1)?;
                }
            }
        }
        for value in sorted(res.fonts.clone()) {
            let Object::Reference(r) = value else { continue };
            let Some(font) = self.read(r.id) else { continue };
            let Some(font) = dict_of(&font) else { continue };
            if font.get("Subtype").and_then(|s| s.as_name()) != Some("Type3") {
                continue;
            }
            let own = match font.get("Resources") {
                Some(value) => self.resources(r.id, value).unwrap_or_else(|| res.clone()),
                None => res.clone(),
            };
            let Some(procs) = font.get("CharProcs").and_then(|v| self.dict_value(v)) else {
                continue;
            };
            for proc in sorted(procs) {
                if let Object::Reference(p) = proc {
                    self.stream(p.id, &own, poison, depth + 1)?;
                }
            }
            self.context(&own, poison, depth + 1)?;
        }
        Ok(())
    }

    /// Walks stream `id`, a form or similar, against its own resources or
    /// `inherited`.
    fn stream(&mut self, id: u32, inherited: &Rc<Res>, poison: bool, depth: usize) -> Result<()> {
        self.cancel.check()?;
        if depth > MAX_NESTING {
            self.disabled = true;
            return Ok(());
        }
        let Some(obj) = self.read(id) else { return Ok(()) };
        let Some(dict) = dict_of(&obj) else { return Ok(()) };
        let res = match dict.get("Resources") {
            Some(value) => self.resources(id, value).unwrap_or_else(|| inherited.clone()),
            None => inherited.clone(),
        };
        if !self.visited.insert((id, res.id, poison)) {
            return Ok(());
        }
        let Ok(reader) = decoded_reader(&obj) else {
            self.disabled = true;
            return Ok(());
        };
        let ops = self.operators(reader, &res, None);
        self.run(ops, &res, poison)?;
        self.context(&res, poison, depth)
    }

    /// The parts of a page's `/Contents`, and whether it is one stream.
    fn contents(&self, value: &Object) -> Option<(Vec<Object>, bool)> {
        let items = match value {
            Object::Reference(r) => match self.read(r.id)? {
                stream @ Object::Stream { .. } => return Some((vec![stream], true)),
                Object::Array(items) => items,
                _ => return None,
            },
            Object::Array(items) => items.clone(),
            _ => return None,
        };
        let mut parts = Vec::with_capacity(items.len());
        for item in &items {
            match item {
                Object::Null => {},
                other => parts.push(self.read(other.as_reference()?.id)?),
            }
        }
        Some((parts, false))
    }

    fn page(&mut self, id: u32, held: bool) -> Result<()> {
        self.cancel.check()?;
        let Some(obj) = self.read(id) else { return Ok(()) };
        let Some(dict) = dict_of(&obj) else { return Ok(()) };
        let Some(res) = self.page_resources(id) else {
            // A page with no resources shows no font; one with text is damage.
            if dict.get("Contents").is_some() {
                self.disabled = true;
            }
            return Ok(());
        };
        if let Some(contents) = dict.get("Contents") {
            match self.contents(contents) {
                Some((parts, _)) if parts.is_empty() => {},
                Some((mut parts, single)) => {
                    let unreadable = Arc::new(AtomicU64::new(0));
                    let source: Box<dyn Read> = if single {
                        match decoded_reader(&parts.remove(0)) {
                            Ok(reader) => {
                                Box::new(PartsReader::of_one(reader, Arc::clone(&unreadable)))
                            },
                            Err(_) => {
                                self.disabled = true;
                                return Ok(());
                            },
                        }
                    } else {
                        Box::new(PartsReader::of_array(parts, Arc::clone(&unreadable)))
                    };
                    let ops = self.operators(source, &res, Some(unreadable));
                    self.run(ops, &res, held)?;
                },
                None => self.disabled = true,
            }
        }
        self.context(&res, held, 0)?;
        self.annotations(dict, &res, held)
    }

    fn annotations(&mut self, page: &Dict, res: &Rc<Res>, held: bool) -> Result<()> {
        let Some(Object::Array(annots)) = page.get("Annots").and_then(|v| self.resolve(v)) else {
            return Ok(());
        };
        for annot in &annots {
            let Some(annot) = self.dict_value(annot) else { continue };
            let poison = held
                || annot.get("Subtype").and_then(|s| s.as_name()) == Some("Widget")
                || annot.contains_key("FT");
            let Some(ap) = annot.get("AP").and_then(|v| self.dict_value(v)) else { continue };
            for key in ["N", "D", "R"] {
                let Some(entry) = ap.get(key) else { continue };
                let mut streams = Vec::new();
                match self.resolve(entry) {
                    Some(Object::Stream { .. }) => {
                        streams.extend(entry.as_reference().map(|r| r.id));
                    },
                    Some(Object::Dictionary(states)) => {
                        streams.extend(states.values().filter_map(|v| v.as_reference()).map(|r| r.id));
                    },
                    _ => {},
                }
                for id in streams {
                    self.stream(id, res, poison, 1)?;
                }
            }
        }
        Ok(())
    }

    /// Poisons the fonts a resource dictionary no walk reached holds, the
    /// fonts an `ExtGState` names and those of every `/DR`.
    fn scan(&mut self, ids: Vec<u32>) -> Result<()> {
        for id in ids {
            self.cancel.check()?;
            let Some(obj) = self.read(id) else { continue };
            let Some(dict) = dict_of(&obj) else { continue };
            self.scan_dict(dict, Some(id), true);
        }
        Ok(())
    }

    fn scan_dict(&mut self, dict: &Dict, owner: Option<u32>, top: bool) {
        if let Some(value) = dict.get("Font") {
            self.scan_fonts(value, owner);
        }
        if let Some(value) = dict.get("DR") {
            if let Some(resources) = self.dict_value(value) {
                if let Some(map) = resources.get("Font").and_then(|v| self.dict_value(v)) {
                    self.poison_map(&map);
                }
            }
        }
        for (key, value) in dict {
            if key == "Font" {
                continue;
            }
            let inner = if top && key == "Resources" { owner } else { None };
            self.scan_value(value, inner);
        }
    }

    fn scan_value(&mut self, value: &Object, owner: Option<u32>) {
        match value {
            Object::Dictionary(dict) => self.scan_dict(dict, owner, false),
            Object::Array(items) => items.iter().for_each(|item| self.scan_value(item, None)),
            _ => {},
        }
    }

    fn scan_fonts(&mut self, value: &Object, owner: Option<u32>) {
        let (map, holder) = match value {
            Object::Dictionary(map) => (map.clone(), owner.unwrap_or(NEVER)),
            Object::Reference(r) => match self.dict_value(value) {
                Some(map) => (map, r.id),
                None => return,
            },
            Object::Array(items) => {
                // an ExtGState's [font size]
                if let Some(first) = items.first() {
                    self.poison_font_value(first);
                }
                return;
            },
            _ => return,
        };
        if self.walked.contains(&holder) {
            return;
        }
        self.poison_map(&map);
    }

    /// The replacement stream of each program the glyph sets let shrink.
    fn finish(
        &mut self,
        aliases: &HashMap<u32, u32>,
        level: u32,
        own_ids: &HashSet<u32>,
    ) -> HashMap<u32, Object> {
        let mut sets: HashMap<u32, BTreeSet<u16>> = HashMap::new();
        let mut kinds: HashMap<u32, Kind> = HashMap::new();
        let mut poisoned: HashSet<u32> = HashSet::new();
        for (&file, set) in &self.glyphs {
            let target = aliases.get(&file).copied().unwrap_or(file);
            sets.entry(target).or_default().extend(set.iter().copied());
            if let Some(&kind) = self.kinds.get(&file) {
                if *kinds.entry(target).or_insert(kind) != kind {
                    poisoned.insert(target);
                }
            }
        }
        for &file in &self.poisoned {
            poisoned.insert(aliases.get(&file).copied().unwrap_or(file));
            poisoned.insert(file);
        }
        // A twin outside this document is a stream of a merged source.
        for (duplicate, kept) in aliases {
            if !own_ids.contains(duplicate) {
                poisoned.insert(*kept);
            }
        }

        let mut out = HashMap::new();
        let mut ids: Vec<u32> = sets.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            if self.cancel.check().is_err() {
                break;
            }
            let (Some(set), Some(&kind)) = (sets.get(&id), kinds.get(&id)) else { continue };
            if poisoned.contains(&id)
                || !own_ids.contains(&id)
                || set.is_empty()
                || self.overrides.contains_key(&id)
                || self.staged.contains_key(&id)
            {
                continue;
            }
            if let Some(replacement) = self.subset(id, kind, set, level) {
                out.insert(id, replacement);
            }
        }
        out
    }

    fn subset(&self, id: u32, kind: Kind, set: &BTreeSet<u16>, level: u32) -> Option<Object> {
        let Object::Stream { dict, body } = self.read(id)? else { return None };
        let mut program = Vec::new();
        decoded_reader(&Object::Stream { dict: dict.clone(), body: body.clone() })
            .ok()?
            .read_to_end(&mut program)
            .ok()?;
        let cut = match kind {
            Kind::TrueTypeSimple | Kind::TrueTypeCid => truetype::subset(&program, set)?,
            Kind::CffCid => cff::subset(&program, set)?,
        };
        // What the save stores for the program without the plan: what the
        // source holds, or the program deflated, whichever is smaller.
        let deflated = |bytes: &[u8]| -> Option<Vec<u8>> {
            let mut out = Vec::new();
            crate::host::deflate::recode(std::io::Cursor::new(bytes), false, level, &mut out).ok()?;
            Some(out)
        };
        let packed = deflated(&cut)?;
        let baseline = (body.len()).min(deflated(&program)?.len() as u64);
        if packed.len() as u64 >= baseline {
            return None;
        }
        let mut dict = dict;
        dict.remove("DecodeParms");
        dict.remove("DL");
        dict.insert("Filter".to_string(), Object::Name("FlateDecode".to_string()));
        dict.insert("Length".to_string(), Object::Integer(packed.len() as i64));
        if dict.contains_key("Length1") {
            dict.insert("Length1".to_string(), Object::Integer(cut.len() as i64));
        }
        Some(Object::stream(dict, packed))
    }
}

impl Entry {
    /// Every glyph `code` can show in this font.
    fn reach(&mut self, code: u16) -> Vec<u16> {
        if let Some(hit) = self.memo.get(&code) {
            return hit.clone();
        }
        let mut out: BTreeSet<u16> = BTreeSet::new();
        let info = &self.info;
        if self.kind != Kind::TrueTypeSimple {
            let spec = match &info.cid_to_gid_map {
                Some(CIDToGIDMap::Identity) => code,
                Some(CIDToGIDMap::Explicit(map)) => *map.get(code as usize).unwrap_or(&code),
                None => info
                    .cff_cid_to_gid
                    .as_ref()
                    .and_then(|m| m.get(&code).copied())
                    .unwrap_or(code),
            };
            out.insert(spec);
            out.insert(code);
        }
        if self.kind != Kind::CffCid {
            if let Some(data) = &info.embedded_font_data {
                if let Ok(face) = ttf_parser::Face::parse(data, 0) {
                    let mut keys: Vec<u32> = Vec::new();
                    if self.kind == Kind::TrueTypeSimple {
                        keys.extend([code as u32, 0xF000 | code as u32]);
                    }
                    let text = info.char_to_unicode(code as u32).unwrap_or_default();
                    keys.extend(text.chars().map(|c| c as u32));
                    if let Some(extra) = u8::try_from(code).ok().and_then(|c| info.multi_char_map.get(&c)) {
                        keys.extend(extra.chars().map(|c| c as u32));
                    }
                    if let Some(cmap) = face.tables().cmap {
                        for key in &keys {
                            for sub in cmap.subtables {
                                if let Some(gid) = sub.glyph_index(*key) {
                                    out.insert(gid.0);
                                }
                            }
                        }
                    }
                    if let Some(name) = u8::try_from(code).ok().and_then(|c| info.diff_glyph_names.get(&c)) {
                        if let Some(gid) = face.glyph_index_by_name(name) {
                            out.insert(gid.0);
                        }
                    }
                    if let Some(map) = info.truetype_cmap() {
                        if let Some(gid) = map.code_to_gid(code) {
                            out.insert(gid);
                        }
                    }
                }
            }
        }
        let out: Vec<u16> = out.into_iter().collect();
        self.memo.insert(code, out.clone());
        out
    }
}

/// The replacement streams of the font programs of `scope`'s pages that the
/// glyphs shown let shrink, by object id.
pub(crate) fn plan(scope: Scope<'_>, cancel: &CancelCheck) -> Result<HashMap<u32, Object>> {
    let Scope { doc, staged, overrides, aliases, pages, level } = scope;
    let mut walker = Walker {
        doc,
        staged,
        overrides,
        fonts: HashMap::new(),
        kinds: HashMap::new(),
        glyphs: HashMap::new(),
        poisoned: HashSet::new(),
        walked: HashSet::new(),
        resources: HashMap::new(),
        visited: HashSet::new(),
        done: HashSet::new(),
        disabled: false,
        cancel: cancel.clone(),
    };
    // Nothing to cut, nothing to read: no content is opened.
    let mut has_program = false;
    for id in doc.all_object_ids() {
        cancel.check()?;
        if let Some(obj) = walker.read(id) {
            has_program |= dict_of(&obj)
                .is_some_and(|d| d.contains_key("FontFile2") || d.contains_key("FontFile3"));
        }
        if has_program {
            break;
        }
    }
    if !has_program {
        return Ok(HashMap::new());
    }
    for (page, held) in pages {
        walker.page(page, held)?;
        if walker.disabled {
            return Ok(HashMap::new());
        }
    }
    let mut ids = doc.all_object_ids();
    let source_ids: HashSet<u32> = ids.iter().copied().collect();
    ids.extend(staged.keys().copied().filter(|id| !source_ids.contains(id)));
    ids.extend(overrides.keys().copied().filter(|id| !source_ids.contains(id)));
    ids.sort_unstable();
    ids.dedup();
    let mut own_ids = source_ids;
    own_ids.extend(staged.keys().copied());
    walker.scan(ids)?;
    Ok(walker.finish(aliases, level, &own_ids))
}
