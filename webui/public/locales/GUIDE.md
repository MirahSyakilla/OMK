# Translation Guide

Every string the WebUI shows lives in `strings/en.xml`, keyed by name. The other
files in `strings/` are translations of exactly that key set.

## Adding a language

1. Copy `template.xml` to `strings/<code>.xml` and translate the values. Keep the
   `name` attributes exactly as they are, and keep every `%s`, `%d` and `%1$d`
   placeholder intact and in the same order.
2. Add the language to `languages.json` under its own name, for example
   `"zh-CN": "简体中文"`. The picker lists exactly what is in that file, so a
   language that is not listed cannot be selected.
3. Fill in every key. A missing key does not fail visibly: it silently falls back
   to English, which reads as a broken translation rather than an incomplete one.
   Prefer leaving a language out until it is complete.

## Checking a translation

`write_locale.py` validates a file and is worth running before committing:

```sh
python3 write_locale.py <code> <translations.json>
```

It fails on a missing or unknown key, a changed placeholder, an empty value,
stray leading or trailing whitespace, and text from a foreign script. The script
check is skipped for CJK locales, where Han characters are expected.

## Naming

`en.xml` is the source of truth. If a key is added or removed there, every other
locale needs the same change or it will fail the check above.
