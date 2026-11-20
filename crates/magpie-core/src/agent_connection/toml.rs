//! TOML syntax stays in toml_edit; unsupported scalar types remain opaque to ownership.
use super::{Error, Value};
use toml_edit::{DocumentMut, Item, TableLike};

fn document(bytes: &[u8]) -> Result<DocumentMut, Error> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(Error::TooLarge);
    }
    std::str::from_utf8(bytes)
        .map_err(|_| Error::InvalidUtf8)?
        .parse()
        .map_err(|_| Error::UnsupportedShape)
}

fn table_value(table: &dyn TableLike, depth: usize) -> Result<Value, Error> {
    if depth > 128 {
        return Err(Error::UnsupportedShape);
    }
    table
        .iter()
        .filter(|(_, item)| !item.is_none())
        .map(|(key, item)| Ok((key.to_owned(), item_value(item, depth + 1)?)))
        .collect::<Result<serde_json::Map<_, _>, _>>()
        .map(Value::Object)
}
fn item_value(item: &Item, depth: usize) -> Result<Value, Error> {
    if let Some(table) = item.as_table_like() {
        table_value(table, depth)
    } else if let Some(value) = item.as_value() {
        scalar_value(value, depth)
    } else if let Some(array) = item.as_array_of_tables() {
        array
            .iter()
            .map(|table| table_value(table, depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    } else {
        Err(Error::UnsupportedShape)
    }
}
fn scalar_value(value: &toml_edit::Value, depth: usize) -> Result<Value, Error> {
    if depth > 128 {
        return Err(Error::UnsupportedShape);
    }
    Ok(match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(n) => Value::Number((*n.value()).into()),
        toml_edit::Value::Float(n) => serde_json::Number::from_f64(*n.value())
            .map(Value::Number)
            .unwrap_or_else(|| serde_json::json!({"$toml_nonfinite": n.value().to_string()})),
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        // Never mistake a timestamp for an editable string or nonfinite number for null.
        toml_edit::Value::Datetime(d) => {
            serde_json::json!({"$toml_datetime": d.value().to_string()})
        }
        toml_edit::Value::Array(array) => Value::Array(
            array
                .iter()
                .map(|v| scalar_value(v, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        toml_edit::Value::InlineTable(table) => table_value(table, depth + 1)?,
    })
}

pub(super) fn parse(bytes: &[u8]) -> Result<Value, Error> {
    table_value(document(bytes)?.as_table(), 0)
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn scalar(value: &Value) -> Result<toml_edit::Value, Error> {
    Ok(match value {
        Value::String(s) => toml_edit::Value::from(s.as_str()),
        Value::Bool(b) => toml_edit::Value::from(*b),
        Value::Number(n) if n.is_f64() => {
            toml_edit::Value::from(n.as_f64().ok_or(Error::UnsupportedShape)?)
        }
        Value::Number(n) => toml_edit::Value::from(n.as_i64().ok_or(Error::UnsupportedShape)?),
        _ => return Err(Error::UnsupportedShape),
    })
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn keep_comments(decor: &toml_edit::Decor, comments: &mut String) {
    for raw in [decor.prefix(), decor.suffix()].into_iter().flatten() {
        if let Some(text) = raw.as_str().filter(|s| s.contains('#')) {
            comments.push('\n');
            comments.push_str(text);
            comments.push('\n');
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn edit(
    table: &mut dyn TableLike,
    keys: &[String],
    value: Option<&Value>,
    comments: &mut String,
) -> Result<(), Error> {
    let (key, rest) = keys.split_first().ok_or(Error::UnsupportedShape)?;
    if !rest.is_empty() {
        if !table.contains_key(key) {
            if value.is_none() {
                return Ok(());
            }
            // Children of inline tables must themselves remain inline.
            table.insert(
                key,
                Item::Value(toml_edit::Value::InlineTable(toml_edit::InlineTable::new())),
            );
        }
        return edit(
            table
                .get_mut(key)
                .and_then(Item::as_table_like_mut)
                .ok_or(Error::UnsupportedShape)?,
            rest,
            value,
            comments,
        );
    }
    if let Some(value) = value {
        let mut next = scalar(value)?;
        if let Some(old) = table.get_mut(key) {
            let old = old.as_value_mut().ok_or(Error::UnsupportedShape)?;
            *next.decor_mut() = old.decor().clone();
            *old = next;
        } else {
            table.insert(key, Item::Value(next));
        }
    } else {
        if let Some((key, item)) = table.get_key_value(key) {
            keep_comments(key.leaf_decor(), comments);
            keep_comments(key.dotted_decor(), comments);
            if let Some(value) = item.as_value() {
                keep_comments(value.decor(), comments);
                if let Some(table) = value.as_inline_table() {
                    if let Some(text) = table.trailing().as_str().filter(|s| s.contains('#')) {
                        comments.push('\n');
                        comments.push_str(text);
                        comments.push('\n');
                    }
                }
            } else if let Some(table) = item.as_table() {
                keep_comments(table.decor(), comments);
            }
        }
        table.remove(key);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub(super) fn set(bytes: &[u8], keys: &[String], value: Option<&Value>) -> Result<Vec<u8>, Error> {
    let mut doc = document(bytes)?;
    let mut comments = String::new();
    edit(doc.as_table_mut(), keys, value, &mut comments)?;
    if !comments.is_empty() {
        comments.push_str(doc.trailing().as_str().unwrap_or_default());
        doc.set_trailing(comments);
    }
    let result = doc.to_string().into_bytes();
    parse(&result)?;
    Ok(result)
}
