"""Writes the Northwind fixture site and demo workspace suites for documentation screenshots.

Usage: python fixture.py <shots directory>
Creates <shots>/site/{v1,v2}/*.html, served on http://127.0.0.1:4380/ by
`python -m http.server 4380 --bind 127.0.0.1 --directory <shots>/site`, and the suites of
<shots>/northwind-qa (run `regressify init` there first). Reference pages are v1 and test pages
v2, so every suite has one visible difference to review.

The Help Center suite reaches the same server as `localhost`, a second host, through the
`{HELP}` placeholder in common/_replacement-profiles.yaml, so the environments-and-domains
page can show one profile serving two domains. The store suites keep their own
`urlReplacements`, so screenshots of their Source stay unchanged.
"""
import os
import sys

root = sys.argv[1]
site = os.path.join(root, 'site')
workspace = os.path.join(root, 'northwind-qa')

PAGE = """<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Northwind Store - {title}</title>
<style>
*{{box-sizing:border-box}}body{{margin:0;font-family:Segoe UI,system-ui,sans-serif;background:#f6f7f9;color:#1d2433}}
header{{display:flex;align-items:center;justify-content:space-between;padding:18px 48px;background:#fff;border-bottom:1px solid #e3e6ea}}
.logo{{font-weight:700;font-size:20px;color:#2b6cb0}}nav a{{margin-left:24px;color:#4a5568;text-decoration:none}}
main{{max-width:1040px;margin:40px auto;display:grid;grid-template-columns:1.4fr 1fr;gap:32px;padding:0 24px}}
.card{{background:#fff;border:1px solid #e3e6ea;border-radius:10px;padding:28px}}
h1{{margin:0 0 20px;font-size:28px}}label{{display:block;font-size:14px;margin:14px 0 6px;color:#4a5568}}
input{{width:100%;padding:10px 12px;border:1px solid #cbd2d9;border-radius:6px;font-size:15px}}
.row{{display:flex;justify-content:space-between;padding:10px 0;border-bottom:1px solid #eef0f3}}
button{{margin-top:20px;width:100%;padding:14px;border:0;border-radius:8px;background:{button};color:#fff;font-size:16px;font-weight:600}}
.banner{{background:#fff4d6;border:1px solid #f0d58a;padding:12px 16px;border-radius:8px;margin-bottom:16px;display:{banner}}}
</style></head><body>
<header><span class="logo">Northwind</span><nav><a href="#">Shop</a><a href="#">Deals</a><a href="#">Account</a><a href="#">Cart (2)</a></nav></header>
<main><section class="card"><div class="banner">Free shipping on orders over $50</div><h1>{title}</h1>
<label>Email</label><input value="alex@example.com"><label>Full name</label><input value="Alex Morgan">
<label>Address</label><input value="12 Harbour Street"></section>
<aside class="card"><h2 style="margin-top:0">Summary</h2><div class="row"><span>Trail backpack</span><span>$64.00</span></div>
<div class="row"><span>Shipping</span><span>{shipping}</span></div><button id="pay">Continue</button></aside></main></body></html>
"""

SUITES = {
    'checkout': ('Checkout', 'Checkout page'),
    'account': ('Account', 'Your account'),
    'catalog': ('Catalog', 'Outdoor gear'),
    'search': ('Search', 'Search results'),
}
HELP_SUITE = ('help-center', 'Help Center', 'How can we help?')


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'w', encoding='utf-8', newline='\n') as handle:
        handle.write(text)


PAGES = {**{suite: title for suite, (_, title) in SUITES.items()}, HELP_SUITE[0]: HELP_SUITE[2]}
for version, button, banner, shipping in [('v1', '#2b6cb0', 'none', '$6.00'), ('v2', '#c05621', 'block', 'Free')]:
    for page, title in PAGES.items():
        write(os.path.join(site, version, f'{page}.html'),
              PAGE.format(title=title, button=button, banner=banner, shipping=shipping))

for suite, (display, title) in SUITES.items():
    write(os.path.join(workspace, 'visual_tests', f'{suite}.tests.yaml'), f"""displayName: {display}
viewportNames: [desktop]
urlReplacements:
  - ref: http://127.0.0.1:4380/v1/
    test: http://127.0.0.1:4380/v2/
scenarios:
  - url: http://127.0.0.1:4380/v1/{suite}.html
    label: {title}
""")

# Overwrites the empty default profile that `regressify init` creates.
write(os.path.join(workspace, 'common', '_replacement-profiles.yaml'), """profiles:
  default:
    # Store pages
    - find: "{STORE}"
      ref: http://127.0.0.1:4380/v1
      test: http://127.0.0.1:4380/v2
    # Help Center pages on a second domain
    - find: "{HELP}"
      ref: http://localhost:4380/v1
      test: http://localhost:4380/v2
""")
help_suite, help_display, help_title = HELP_SUITE
write(os.path.join(workspace, 'visual_tests', f'{help_suite}.tests.yaml'), f"""displayName: {help_display}
viewportNames: [desktop]
scenarios:
  - url: "{{HELP}}/{help_suite}.html"
    label: {help_title}
""")
print(f'Fixture written under {root}')
