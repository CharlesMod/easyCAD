#!/usr/bin/env python3
"""Generate assets/icon/easycad.svg: an isometric enclosure (open base, lid
lifted off, a green PCB inside) on a dark rounded tile."""
import sys
S = 512
cx, cy = 256, 196          # screen position of the base's back corner at z=0
s, h, gap, t, w = 178, 92, 70, 24, 15   # box side, base height, lid gap, lid thickness, wall
k = 0.866

def P(x, y, z):
    return (cx + (x - y) * k, cy + (x + y) * 0.5 - z)

def poly(pts, fill, extra=""):
    d = " ".join(f"{x:.1f},{y:.1f}" for x, y in pts)
    return f'<polygon points="{d}" fill="{fill}" {extra}/>'

# move everything down so the lid sits in the tile
dz = -64
def Q(x, y, z): return P(x, y, z + dz)

out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {S} {S}" width="{S}" height="{S}">',
 '<defs><linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">'
 '<stop offset="0" stop-color="#27303b"/><stop offset="1" stop-color="#151a20"/></linearGradient>',
 f'<clipPath id="open"><polygon points="{" ".join(f"{x:.1f},{y:.1f}" for x,y in [Q(w,w,h),Q(s-w,w,h),Q(s-w,s-w,h),Q(w,s-w,h)])}"/></clipPath></defs>',
 '<rect x="16" y="16" width="480" height="480" rx="108" fill="url(#bg)"/>']
# base: outer faces
out.append(poly([Q(s,0,0),Q(s,s,0),Q(s,s,h),Q(s,0,h)], "#4c9bff"))          # right face
out.append(poly([Q(0,s,0),Q(s,s,0),Q(s,s,h),Q(0,s,h)], "#2f6fc4"))          # left face
out.append(poly([Q(0,0,h),Q(s,0,h),Q(s,s,h),Q(0,s,h)], "#8fc0ff"))          # rim (top)
# inside, clipped to the opening
inner = []
inner.append(poly([Q(w,w,h),Q(s-w,w,h),Q(s-w,s-w,h),Q(w,s-w,h)], "#10213a"))
inner.append(poly([Q(w,w,h),Q(s-w,w,h),Q(s-w,w,w),Q(w,w,w)], "#244e86"))     # back-right wall inner face
inner.append(poly([Q(w,w,h),Q(w,s-w,h),Q(w,s-w,w),Q(w,w,w)], "#1a3a66"))     # back-left wall inner face
# PCB inside: a green board on standoffs
pz, m = 44, 34
inner.append(poly([Q(m,m,pz),Q(s-m,m,pz),Q(s-m,s-m,pz),Q(m,s-m,pz)], "#2fa36b"))
inner.append(poly([Q(s-m,m,pz),Q(s-m,s-m,pz),Q(s-m,s-m,pz-7),Q(s-m,m,pz-7)], "#1f7a4f"))
inner.append(poly([Q(m,s-m,pz),Q(s-m,s-m,pz),Q(s-m,s-m,pz-7),Q(m,s-m,pz-7)], "#186040"))
c0 = 64
inner.append(poly([Q(c0,c0,pz),Q(c0+44,c0,pz),Q(c0+44,c0+44,pz),Q(c0,c0+44,pz)], "#1b2027"))   # chip
out.append('<g clip-path="url(#open)">' + "".join(inner) + "</g>")
# lid
z0 = h + gap
out.append(poly([Q(s,0,z0),Q(s,s,z0),Q(s,s,z0+t),Q(s,0,z0+t)], "#4c9bff"))
out.append(poly([Q(0,s,z0),Q(s,s,z0),Q(s,s,z0+t),Q(0,s,z0+t)], "#2f6fc4"))
out.append(poly([Q(0,0,z0+t),Q(s,0,z0+t),Q(s,s,z0+t),Q(0,s,z0+t)], "#a9d0ff"))
# orange lip under the lid (the parting joint's tongue)
l = w + 4
out.append(poly([Q(l,s-l,z0),Q(s-l,s-l,z0),Q(s-l,s-l,z0-12),Q(l,s-l,z0-12)], "#e8892f"))
out.append(poly([Q(s-l,l,z0),Q(s-l,s-l,z0),Q(s-l,s-l,z0-12),Q(s-l,l,z0-12)], "#f5a55a"))
out.append("</svg>")
open(sys.argv[1], "w").write("\n".join(out) + "\n")
