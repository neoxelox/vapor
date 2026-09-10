"""Outlines the Vapor wordmark from Nimbus Sans Bold into source/marketing.
Only needed to change the typography; the generated paths are committed.
Requires FontTools and a local copy of the font (set VAPOR_WORDMARK_FONT).
"""
import os
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.boundsPen import BoundsPen
import json
root=Path(__file__).resolve().parents[2]
font=TTFont(os.environ.get('VAPOR_WORDMARK_FONT','/usr/share/fonts/opentype/urw-base35/NimbusSans-Bold.otf'))
glyphs=font.getGlyphSet();cmap=font.getBestCmap();x=0;pieces=[];bounds=[]
for i,ch in enumerate('Vapor'):
    if i==1:x-=68
    if i>1:x-=18
    name=cmap[ord(ch)];glyph=glyphs[name]
    pen=SVGPathPen(glyphs);glyph.draw(TransformPen(pen,(1,0,0,-1,x,0)))
    bp=BoundsPen(glyphs);glyph.draw(TransformPen(bp,(1,0,0,-1,x,0)));bounds.append(bp.bounds)
    pieces.append({'letter':ch,'d':pen.getCommands(),'bounds':bp.bounds})
    x+=font['hmtx'][name][0]
box=[min(b[0] for b in bounds),min(b[1] for b in bounds),max(b[2] for b in bounds),max(b[3] for b in bounds)]
W=box[2]-box[0];H=box[3]-box[1]
obj={'font':'Nimbus Sans Bold','font_version':font['name'].getDebugName(5),'bounds':box,'width':W,'height':H,'glyphs':pieces,'kerning_units':[-68,-18,-18,-18]}
(root/'source/marketing/wordmark-paths.json').write_text(json.dumps(obj,indent=2)+'\n')
paths=''.join('<path d="'+p['d']+'"/>' for p in pieces)
(root/'source/marketing/vapor-wordmark.svg').write_text(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{box[0]-9} {box[1]-9} {W+18} {H+18}" width="{W+18}" height="{H+18}"><g fill="currentColor" stroke="currentColor" stroke-width="18" stroke-linejoin="round">{paths}</g></svg>\n')
print('Wordmark bounds:',box,'aspect:',W/H)
