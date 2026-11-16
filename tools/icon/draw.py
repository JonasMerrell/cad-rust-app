def tile(inner, grid_step=48):
    grid=''.join(f'<line x1="{x}" y1="16" x2="{x}" y2="496"/>' for x in range(40,496,grid_step))+''.join(f'<line x1="16" y1="{y}" x2="496" y2="{y}"/>' for y in range(40,496,grid_step))
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
<defs><linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2a7be4"/><stop offset="1" stop-color="#134fa3"/></linearGradient>
<clipPath id="tc"><rect x="16" y="16" width="480" height="480" rx="108"/></clipPath></defs>
<rect x="16" y="16" width="480" height="480" rx="108" fill="url(#bg)"/>
<g clip-path="url(#tc)" stroke="#fff" stroke-opacity=".09" stroke-width="2">{grid}</g>
{inner}</svg>'''
dot=lambda x,y,r=13:f'<circle cx="{x}" cy="{y}" r="{r}" fill="#ffb020"/>'
ring=lambda x,y:f'<circle cx="{x}" cy="{y}" r="10" fill="#ffffff" stroke="#1a6fd9" stroke-width="5"/>'
W=24
oc,oa,od=24,40,56
word=f'''<g transform="translate(256 262) scale(0.84) translate({-(84+451+od)/2:.1f} -257)">
<line x1="30" y1="316" x2="{480+od}" y2="316" stroke="#fff" stroke-opacity=".35" stroke-width="5" stroke-dasharray="14 12"/>
<g fill="none" stroke="#fff" stroke-width="{W}" stroke-linecap="round" stroke-linejoin="round">
<path d="M84 316 V270 A42 42 0 0 1 126 228"/>
<path transform="translate({oc} 0)" d="M231 215 A58 58 0 1 0 231 297"/>
<path transform="translate({oa} 0)" d="M256 316 L300 198 L344 316 M271 276 H329"/>
<path transform="translate({od} 0)" d="M370 198 H392 A59 59 0 0 1 392 316 H370 Z"/>
</g>
<g stroke="#bcd8ff" stroke-width="5" fill="none"><path d="M84 384 H{451+od} M84 370 v28 M{451+od} 370 v28"/></g>
<path d="M84 384 l16 -7 v14 z M{451+od} 384 l-16 -7 v14 z" fill="#bcd8ff"/>
{dot(84,316)}{dot(126,228)}{dot(231+oc,215)}{dot(231+oc,297)}{dot(256+oa,316)}{dot(344+oa,316)}{ring(300+oa,198)}{ring(370+od,198)}{ring(370+od,316)}</g>'''
open('rcad.svg','w').write(tile(word))
# small-size variant: just the r (concept B, tidied)
small=f'''<g fill="none" stroke="#fff" stroke-linecap="round"><path d="M200 392 V262 A112 112 0 0 1 336 150" stroke-width="56"/></g>{dot(200,392,26)}{dot(336,150,26)}'''
open('rcad-small.svg','w').write(tile(small,96))
