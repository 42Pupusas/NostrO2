use js_sys::wasm_bindgen::JsValue;
use js_sys::{Array, Object, Reflect};

use crate::note::NostrNote;
use crate::tags::NostrTags;
use crate::view::NostrNoteView;

fn err(msg: &str) -> JsValue {
    JsValue::from(js_sys::Error::new(msg))
}

fn set(obj: &Object, key: &str, val: &JsValue) {
    Reflect::set(obj, &JsValue::from_str(key), val).unwrap();
}

fn get(obj: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    Reflect::get(obj, &JsValue::from_str(key)).map_err(|_| err(&format!("missing field: {key}")))
}

fn get_string(obj: &JsValue, key: &str) -> Result<String, JsValue> {
    get(obj, key)?
        .as_string()
        .ok_or_else(|| err(&format!("{key}: expected string")))
}

fn get_opt_string(obj: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let val = get(obj, key)?;
    if val.is_undefined() || val.is_null() {
        return Ok(None);
    }
    Ok(val.as_string())
}

fn get_f64(obj: &JsValue, key: &str) -> Result<f64, JsValue> {
    get(obj, key)?
        .as_f64()
        .ok_or_else(|| err(&format!("{key}: expected number")))
}

fn tags_to_js<'a, R, C>(rows: impl Iterator<Item = R>) -> JsValue
where
    R: IntoIterator<Item = &'a C>,
    C: AsRef<str> + 'a,
{
    let outer = Array::new();
    for row in rows {
        let inner = Array::new();
        for cell in row {
            inner.push(&JsValue::from_str(cell.as_ref()));
        }
        outer.push(&inner);
    }
    outer.into()
}

fn tags_from_js(val: &JsValue) -> Result<NostrTags, JsValue> {
    let outer = Array::from(val);
    let mut cells = Vec::new();
    let mut offsets: Vec<u32> = vec![0];
    for i in 0..outer.length() {
        let inner = Array::from(&outer.get(i));
        for j in 0..inner.length() {
            cells.push(
                inner
                    .get(j)
                    .as_string()
                    .ok_or_else(|| err(&format!("tags[{i}][{j}]: expected string")))?,
            );
        }
        #[allow(clippy::cast_possible_truncation)]
        offsets.push(cells.len() as u32);
    }
    Ok(NostrTags { cells, offsets })
}

fn note_to_obj(
    pubkey: &str,
    created_at: i64,
    kind: u32,
    tags_js: &JsValue,
    content: &str,
    id: Option<&str>,
    sig: Option<&str>,
) -> JsValue {
    let obj = Object::new();
    set(&obj, "pubkey", &JsValue::from_str(pubkey));
    #[allow(clippy::cast_precision_loss)]
    set(&obj, "created_at", &JsValue::from_f64(created_at as f64));
    set(&obj, "kind", &JsValue::from_f64(f64::from(kind)));
    set(&obj, "tags", tags_js);
    set(&obj, "content", &JsValue::from_str(content));
    if let Some(id) = id {
        set(&obj, "id", &JsValue::from_str(id));
    }
    if let Some(sig) = sig {
        set(&obj, "sig", &JsValue::from_str(sig));
    }
    obj.into()
}

impl From<NostrNote> for JsValue {
    fn from(note: NostrNote) -> Self {
        let tags_js = tags_to_js(note.tags.iter());
        note_to_obj(
            &note.pubkey,
            note.created_at,
            note.kind,
            &tags_js,
            &note.content,
            note.id.as_deref(),
            note.sig.as_deref(),
        )
    }
}

impl From<&NostrNote> for JsValue {
    fn from(note: &NostrNote) -> Self {
        let tags_js = tags_to_js(note.tags.iter());
        note_to_obj(
            &note.pubkey,
            note.created_at,
            note.kind,
            &tags_js,
            &note.content,
            note.id.as_deref(),
            note.sig.as_deref(),
        )
    }
}

impl From<&NostrNoteView<'_>> for JsValue {
    fn from(view: &NostrNoteView<'_>) -> Self {
        let tags_js = tags_to_js(view.tags.iter());
        note_to_obj(
            view.pubkey.as_ref(),
            view.created_at,
            view.kind,
            &tags_js,
            view.content.as_ref(),
            view.id.as_deref(),
            view.sig.as_deref(),
        )
    }
}

impl TryFrom<JsValue> for NostrNote {
    type Error = JsValue;

    #[allow(unknown_lints, crappy)]
    fn try_from(val: JsValue) -> Result<Self, Self::Error> {
        let pubkey = get_string(&val, "pubkey")?;
        #[allow(clippy::cast_possible_truncation)]
        let created_at = get_f64(&val, "created_at")? as i64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let kind = get_f64(&val, "kind")? as u32;
        let tags = tags_from_js(&get(&val, "tags")?)?;
        let content = get_string(&val, "content")?;
        let id = get_opt_string(&val, "id")?;
        let sig = get_opt_string(&val, "sig")?;
        Ok(Self {
            pubkey,
            created_at,
            kind,
            tags,
            content,
            id,
            sig,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sample_note() -> NostrNote {
        let mut note = NostrNote {
            pubkey: "a".repeat(64),
            created_at: 1_700_000_000,
            kind: 1,
            content: "hello wasm".into(),
            id: Some("b".repeat(64)),
            sig: Some("c".repeat(128)),
            ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        note.tags.add_pubkey_tag(&"d".repeat(64), None);
        note
    }

    #[wasm_bindgen_test]
    fn note_to_js_and_back() {
        let note = sample_note();
        let js: JsValue = note.clone().into();
        let back = NostrNote::try_from(js).unwrap();
        assert_eq!(note, back);
    }

    #[wasm_bindgen_test]
    fn note_ref_to_js_and_back() {
        let note = sample_note();
        let js: JsValue = (&note).into();
        let back = NostrNote::try_from(js).unwrap();
        assert_eq!(note, back);
    }

    #[wasm_bindgen_test]
    fn note_without_id_sig() {
        let note = NostrNote {
            pubkey: "aa".into(),
            created_at: 42,
            kind: 7,
            content: "no id or sig".into(),
            ..Default::default()
        };
        let js: JsValue = note.clone().into();
        let back = NostrNote::try_from(js).unwrap();
        assert_eq!(note, back);
    }

    #[wasm_bindgen_test]
    fn view_to_js_round_trips_through_note() {
        let note = sample_note();
        let json = bourne::to_string(&note).unwrap();
        let view: NostrNoteView<'_> = bourne::parse_str(&json).unwrap();
        let js: JsValue = (&view).into();
        let back = NostrNote::try_from(js).unwrap();
        assert_eq!(note.pubkey, back.pubkey);
        assert_eq!(note.created_at, back.created_at);
        assert_eq!(note.kind, back.kind);
        assert_eq!(note.content, back.content);
        assert_eq!(note.id, back.id);
        assert_eq!(note.sig, back.sig);
        assert_eq!(note.tags.len(), back.tags.len());
    }

    #[wasm_bindgen_test]
    fn rejects_missing_pubkey() {
        let obj = Object::new();
        set(&obj, "created_at", &JsValue::from_f64(1.0));
        set(&obj, "kind", &JsValue::from_f64(1.0));
        set(&obj, "tags", &Array::new().into());
        set(&obj, "content", &JsValue::from_str("hi"));
        assert!(NostrNote::try_from(JsValue::from(obj)).is_err());
    }

    #[wasm_bindgen_test]
    fn rejects_wrong_type_kind() {
        let obj = Object::new();
        set(&obj, "pubkey", &JsValue::from_str("aa"));
        set(&obj, "created_at", &JsValue::from_f64(1.0));
        set(&obj, "kind", &JsValue::from_str("not a number"));
        set(&obj, "tags", &Array::new().into());
        set(&obj, "content", &JsValue::from_str("hi"));
        assert!(NostrNote::try_from(JsValue::from(obj)).is_err());
    }
}
