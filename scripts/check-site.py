#!/usr/bin/env python3
"""Check static site landmarks, local links and fragment targets without a browser."""
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

root = Path(__file__).resolve().parent.parent / 'site'


class Page(HTMLParser):
    def __init__(self, path):
        super().__init__(convert_charrefs=True)
        self.path = path
        self.ids = set()
        self.refs = []
        self.headings = 0
        self.mains = 0
        self.feed(path.read_text(encoding='utf-8'))

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if 'id' in attrs:
            assert attrs['id'] not in self.ids, f'{self.path}: duplicate id'
            self.ids.add(attrs['id'])
        self.headings += tag == 'h1'
        self.mains += tag == 'main'
        for name in ('href', 'src'):
            if name in attrs:
                self.refs.append(attrs[name])


pages = {path.resolve(): Page(path) for path in root.glob('*.html')}
for path, page in pages.items():
    assert page.headings == 1 and page.mains == 1, f'{path}: needs one h1 and main'
    for ref in page.refs:
        url = urlsplit(ref)
        if url.scheme or url.netloc:
            continue
        target = (path.parent / unquote(url.path)).resolve() if url.path else path
        assert target.is_relative_to(root.resolve()), f'{path}: outside site: {ref}'
        assert target.is_file(), f'{path}: missing {ref}'
        if url.fragment and target in pages:
            assert unquote(url.fragment) in pages[target].ids, f'{path}: missing anchor {ref}'
print(f'site check ok: {len(pages)} pages, local links and landmarks')
