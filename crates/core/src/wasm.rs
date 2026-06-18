use std::borrow::Cow;

use js_sys::wasm_bindgen::JsValue;
use js_sys::{Array, Object, Reflect};

use crate::event::NostrEvent;
use crate::note::NostrNote;
use crate::view::NostrNoteView;

// ── Keep JsObj — groups JS Reflect helpers as methods, no free fns ─

struct JsObj(JsValue);

impl JsObj {
    fn new() -> Self { Self(Object::new().into()) }
    fn set(&self, k: &str, v: &JsValue) { Reflect::set(&self.0, &JsValue::from_str(k), v).unwrap(); }

    fn err(msg: &str) -> JsValue { JsValue::from(js_sys::Error::new(msg)) }
    fn get(&self, k: &str) -> Result<JsValue, JsValue> { Reflect::get(&self.0, &JsValue::from_str(k)).map_err(|_| Self::err(&format!("missing field: {k}"))) }
    fn string(&self, k: &str) -> Result<String, JsValue> { self.get(k)?.as_string().ok_or_else(|| Self::err(&format!("{k}: expected string"))) }
    fn opt_string(&self, k: &str) -> Result<Option<String>, JsValue> { let v = self.get(k)?; Ok(if v.is_undefined() || v.is_null() { None } else { v.as_string() }) }
    fn f64(&self, k: &str) -> Result<f64, JsValue> { self.get(k)?.as_f64().ok_or_else(|| Self::err(&format!("{k}: expected number"))) }

    fn into_inner(self) -> JsValue { self.0 }

    fn tags_to_js<'a, R, C>(rows: impl Iterator<Item = R>) -> JsValue
    where R: IntoIterator<Item = &'a C>, C: AsRef<str> + 'a {
        let outer = Array::new();
        for row in rows { let inner = Array::new(); for c in row { inner.push(&JsValue::from_str(c.as_ref())); } outer.push(&inner); }
        outer.into()
    }

    fn tags_from_js(val: &JsValue) -> Result<crate::tags::NostrTags, JsValue> {
        let outer = Array::from(val);
        let mut cells = Vec::new(); let mut offsets: Vec<u32> = vec![0];
        for i in 0..outer.length() {
            let inner = Array::from(&outer.get(i));
            for j in 0..inner.length() { cells.push(inner.get(j).as_string().ok_or_else(|| Self::err(&format!("tags[{i}][{j}]: expected string")))?); }
            #[allow(clippy::cast_possible_truncation)] offsets.push(cells.len() as u32);
        }
        Ok(crate::tags::NostrTags { cells, offsets })
    }

    fn from_fields(pubkey: &str, created_at: i64, kind: u32, content: &str, id: Option<&str>, sig: Option<&str>, tags_js: &JsValue) -> JsValue {
        let obj = Self::new();
        obj.set("pubkey", &JsValue::from_str(pubkey));
        #[allow(clippy::cast_precision_loss)] obj.set("created_at", &JsValue::from_f64(created_at as f64));
        obj.set("kind", &JsValue::from_f64(f64::from(kind)));
        obj.set("tags", tags_js);
        obj.set("content", &JsValue::from_str(content));
        if let Some(id) = id { obj.set("id", &JsValue::from_str(id)); }
        if let Some(sig) = sig { obj.set("sig", &JsValue::from_str(sig)); }
        obj.into_inner()
    }
}

// ── NostrNote / NostrNoteView → JsValue ──────────────────────────

impl From<NostrNote> for JsValue {
    fn from(note: NostrNote) -> Self { (&note).into() }
}

impl From<&NostrNote> for JsValue {
    fn from(note: &NostrNote) -> Self {
        JsObj::from_fields(&note.pubkey, note.created_at, note.kind, &note.content, note.id.as_deref(), note.sig.as_deref(), &JsObj::tags_to_js(note.tags.iter()))
    }
}

impl From<&NostrNoteView<'_>> for JsValue {
    fn from(view: &NostrNoteView<'_>) -> Self {
        JsObj::from_fields(view.pubkey.as_ref(), view.created_at, view.kind, view.content.as_ref(), view.id.as_deref(), view.sig.as_deref(), &JsObj::tags_to_js(view.tags.iter()))
    }
}

// ── JsValue → NostrNote ───────────────────────────────────────────

impl TryFrom<JsValue> for NostrNote {
    type Error = JsValue;
    #[allow(unknown_lints, crappy)]
    fn try_from(val: JsValue) -> Result<Self, Self::Error> {
        let obj = JsObj(val);
        let pubkey = obj.string("pubkey")?;
        #[allow(clippy::cast_possible_truncation)] let created_at = obj.f64("created_at")? as i64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] let kind = obj.f64("kind")? as u32;
        let tags = JsObj::tags_from_js(&obj.get("tags")?)?;
        let content = obj.string("content")?;
        Ok(Self { pubkey, created_at, kind, tags, content, id: obj.opt_string("id")?, sig: obj.opt_string("sig")? })
    }
}

// ── NostrEvent for JsValue (wasm32 only) ─────────────────────────

#[cfg(target_arch = "wasm32")]
impl NostrEvent for JsValue {
    fn pubkey_str(&self) -> Cow<'_, str> { Cow::Owned(Reflect::get(self, &JsValue::from_str("pubkey")).ok().and_then(|v| v.as_string()).unwrap_or_default()) }
    fn created_at(&self) -> i64 { Reflect::get(self, &JsValue::from_str("created_at")).ok().and_then(|v| v.as_f64()).map(|f| f as i64).unwrap_or(0) }
    fn kind(&self) -> u32 { Reflect::get(self, &JsValue::from_str("kind")).ok().and_then(|v| v.as_f64()).map(|f| f as u32).unwrap_or(0) }
    fn content_str(&self) -> Cow<'_, str> { Cow::Owned(Reflect::get(self, &JsValue::from_str("content")).ok().and_then(|v| v.as_string()).unwrap_or_default()) }
    fn id_hex(&self) -> Option<Cow<'_, str>> { Reflect::get(self, &JsValue::from_str("id")).ok().and_then(|v| v.as_string()).map(Cow::Owned) }
    fn sig_hex(&self) -> Option<Cow<'_, str>> { Reflect::get(self, &JsValue::from_str("sig")).ok().and_then(|v| v.as_string()).map(Cow::Owned) }
    fn write_tags<W: bourne::JsonWrite + ?Sized>(&self, sink: &mut W) -> Result<(), W::Error> {
        sink.write_byte(b'[')?;
        if let Ok(tags) = Reflect::get(self, &JsValue::from_str("tags")) {
            let outer = Array::from(&tags);
            for i in 0..outer.length() {
                if i > 0 { sink.write_byte(b',')?; }
                sink.write_byte(b'[')?;
                let inner = Array::from(&outer.get(i));
                for j in 0..inner.length() {
                    if j > 0 { sink.write_byte(b',')?; }
                    if let Some(cell) = inner.get(j).as_string() { sink.write_escaped_str(&cell)?; }
                }
                sink.write_byte(b']')?;
            }
        }
        sink.write_byte(b']')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sample_note() -> NostrNote {
        let mut note = NostrNote { pubkey: "a".repeat(64), created_at: 1_700_000_000, kind: 1, content: "hello wasm".into(), id: Some("b".repeat(64)), sig: Some("c".repeat(128)), ..Default::default() };
        note.tags.add_custom_tag("t", "nostr"); note.tags.add_pubkey_tag(&"d".repeat(64), None);
        note
    }

    #[wasm_bindgen_test] fn note_to_js_and_back() { let n = sample_note(); let js: JsValue = n.clone().into(); assert_eq!(n, NostrNote::try_from(js).unwrap()); }
    #[wasm_bindgen_test] fn note_ref_to_js_and_back() { let n = sample_note(); let js: JsValue = (&n).into(); assert_eq!(n, NostrNote::try_from(js).unwrap()); }
    #[wasm_bindgen_test] fn note_without_id_sig() { let n = NostrNote { pubkey: "aa".into(), created_at: 42, kind: 7, content: "no id or sig".into(), ..Default::default() }; let js: JsValue = n.clone().into(); assert_eq!(n, NostrNote::try_from(js).unwrap()); }
    #[wasm_bindgen_test] fn view_to_js_round_trips_through_note() {
        let n = sample_note(); let json = bourne::to_string(&n).unwrap(); let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        let back = NostrNote::try_from(JsValue::from(&view)).unwrap();
        assert_eq!(n.pubkey, back.pubkey); assert_eq!(n.created_at, back.created_at); assert_eq!(n.kind, back.kind); assert_eq!(n.content, back.content); assert_eq!(n.tags.len(), back.tags.len());
    }
    #[wasm_bindgen_test] fn rejects_missing_pubkey() {
        let obj = JsObj::new(); obj.set("created_at", &JsValue::from_f64(1.0)); obj.set("kind", &JsValue::from_f64(1.0)); obj.set("tags", &Array::new().into()); obj.set("content", &JsValue::from_str("hi"));
        assert!(NostrNote::try_from(obj.into_inner()).is_err());
    }
    #[wasm_bindgen_test] fn rejects_wrong_type_kind() {
        let obj = JsObj::new(); obj.set("pubkey", &JsValue::from_str("aa")); obj.set("created_at", &JsValue::from_f64(1.0)); obj.set("kind", &JsValue::from_str("not a number")); obj.set("tags", &Array::new().into()); obj.set("content", &JsValue::from_str("hi"));
        assert!(NostrNote::try_from(obj.into_inner()).is_err());
    }
}
