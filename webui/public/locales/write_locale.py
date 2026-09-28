#!/usr/bin/env python3
"""Write a locale XML file from a JSON translation map and validate it.

Validation is deliberately strict: a missing key, an unknown key, or a changed
placeholder is an error rather than a warning, because a silent miss falls back to
English at runtime and is only noticed by a user reading the wrong language.
"""
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from xml.sax.saxutils import escape

HERE = Path(__file__).resolve().parent
STRINGS = HERE / 'strings'
PLACEHOLDER = re.compile(r'%(?:\d+\$)?[sdfx]')


def load(path):
    return {s.get('name'): (s.text or '') for s in ET.parse(path).getroot().findall('string')}


def placeholders(text):
    return sorted(PLACEHOLDER.findall(text or ''))


def main(lang, path):
    en = load(STRINGS / 'en.xml')
    tr = json.loads(Path(path).read_text(encoding='utf-8'))

    missing = [k for k in en if k not in tr]
    extra = [k for k in tr if k not in en]
    if missing:
        sys.exit(f'  {lang}: MISSING {len(missing)} keys: {missing}')
    if extra:
        sys.exit(f'  {lang}: UNKNOWN {len(extra)} keys: {extra}')

    bad = [(k, placeholders(en[k]), placeholders(tr[k])) for k in en if placeholders(en[k]) != placeholders(tr[k])]
    if bad:
        sys.exit(f'  {lang}: PLACEHOLDER MISMATCH {bad}')

    # Translations must not start or end with stray whitespace. They must also
    # not carry text from a foreign script, but that check only makes sense where
    # the locale's own script is not the one in the stray text: a Japanese
    # translation is expected to contain Han characters, so the rule is skipped
    # for the CJK locales and applied to the Latin and Cyrillic ones.
    CJK_LOCALES = {'ja', 'ko', 'zh-CN', 'zh-TW'}
    stray = re.compile(r'[\u3040-\u30ff\u4e00-\u9fff\uac00-\ud7af]')

    bad = []
    for k in en:
        value = tr[k]
        if value != value.strip():
            bad.append((k, 'stray whitespace'))
        elif lang not in CJK_LOCALES and stray.search(value):
            bad.append((k, 'foreign script'))
    if bad:
        sys.exit(f'  {lang}: BAD VALUE {bad}')

    empty = [k for k in en if not (tr[k] or '').strip()]
    if empty:
        sys.exit(f'  {lang}: EMPTY {empty}')

    # Untranslated means byte-identical to English for a non-Latin-script locale;
    # report it so a copy-paste slip is visible rather than assumed translated.
    same = [k for k in en if tr[k] == en[k]]
    # Values are escaped here rather than in the translation input, so a literal
    # `&` in "AOSP & Remote" or a `<` in prose cannot produce malformed XML.
    body = ''.join(f'    <string name="{k}">{escape(tr[k])}</string>\n' for k in en)
    (STRINGS / f'{lang}.xml').write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n<resources>\n' + body + '</resources>\n',
        encoding='utf-8',
    )
    ET.parse(STRINGS / f'{lang}.xml')
    print(f'  {lang}: {len(tr)} keys, placeholders ok' + (f', {len(same)} identical to en' if same else ''))


if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
