"""Builds crates/sasacode-gui/assets/icons.ttf: the Lucide icons the window uses, and no others.

usage: python3 icons.py <lucide.ttf> <codepoints.json> <out.ttf>
needs fonttools. The names below are Lucide's; add one here, run this, and add its constant to icon.rs.
"""
import json
import sys

from fontTools import subset
from fontTools.ttLib import TTFont

NAMES = """
square-pen search panel-left chevron-right chevron-down check x terminal file-text pencil file-plus
globe bot brain wrench shield shield-alert shield-check hand gauge box arrow-up square copy
git-branch file-diff circle-alert corner-down-left list-checks clock message-square info
""".split()

source, codepoints, out = sys.argv[1:4]
points = json.load(open(codepoints))
font = TTFont(source)
options = subset.Options()
options.name_IDs = ["*"]
options.notdef_outline = True
options.glyph_names = False
options.layout_features = []
subsetter = subset.Subsetter(options)
subsetter.populate(unicodes=[points[n] for n in NAMES])
subsetter.subset(font)
# Its own family name, so an installed Lucide of another version is never picked instead.
for record in font["name"].names:
    if record.nameID in (1, 3, 4, 6):
        record.string = "sasacode-icons"
font.save(out)
for n in NAMES:
    print(f"pub const {n.upper().replace('-', '_')}: char = '\\u{{{points[n]:x}}}';")
