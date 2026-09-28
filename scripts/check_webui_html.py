#!/usr/bin/env python3
"""Fail on malformed HTML inside the WebUI's template literals.

A tag like `<div class="X"${t('k')}/div>` is valid TypeScript. It compiles, it
bundles, and tsc is happy. The browser then reads the element's own closing tag
as part of its attributes and the layout collapses instead of erroring, so the
damage is only visible on the device. That shipped once: 21 tags on the keybox
screen lost the `>` that closed the attribute, and the whole page came apart.

The check walks each tag, tracking whether it is inside a quoted attribute value,
and flags an interpolation that lands outside one. Inside a value is the normal
case (`id="${id}-back"`); outside one is only legitimate when the interpolation
completes the tag, as in the conditional-attribute idiom
`id="s"${cond ? ' selected' : ''}>`. Anything else after it means the `>` is
missing.
"""
import glob
import pathlib
import re
import sys

TAG = re.compile(r'<[a-zA-Z][a-zA-Z0-9-]*(?:"[^"]*"|\'[^\']*\'|[^>"\'])*>')
INTERP = re.compile(r'\$\{')
# A closing tag, a letter, or a digit after a bare interpolation: none of these
# can legally follow, and each of them means the element's own `</...>` was
# swallowed as attribute text.
ILLEGAL_NEXT = re.compile(r'[A-Za-z0-9/<]')


def offenders(tag: str):
    """Yield the offset of any interpolation that is outside a quoted value."""
    inside = False
    i = 0
    while i < len(tag):
        ch = tag[i]
        if ch == '"':
            inside = not inside
            i += 1
            continue
        if ch == '$' and tag[i:i + 2] == '${':
            close = tag.find('}', i)
            if close == -1:
                return
            nxt = tag[close + 1:close + 2]
            if not inside and ILLEGAL_NEXT.match(nxt or ''):
                yield i
            i = close + 1
            continue
        i += 1


def main() -> int:
    bad = []
    for path in glob.glob('webui/src/**/*.ts', recursive=True):
        src = pathlib.Path(path).read_text()
        for match in TAG.finditer(src):
            tag = match.group(0)
            if any(True for _ in offenders(tag)):
                line = src.count('\n', 0, match.start()) + 1
                bad.append((path, line, tag[:100]))
    if bad:
        print(f'  malformed tags: {len(bad)}')
        for path, line, tag in bad:
            print(f'    {path}:{line}  {tag}')
        return 1
    print('  no malformed interpolated tags')
    return 0


if __name__ == '__main__':
    sys.exit(main())
