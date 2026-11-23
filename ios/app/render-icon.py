#!/usr/bin/env python3
"""Render the existing iOS resting crown to an opaque 1024px PNG; no app or network.
Usage: python3 render-icon.py OUTPUT.png
Uses the retained Rust compiler for pure geometry and Apple's ImageIO for PNG.
"""
import hashlib, json, os, subprocess, sys, tempfile
from pathlib import Path
app = Path(__file__).resolve().parents[1]
source = app / 'native/crates/erlkonig-crown/src/lib.rs'
output = Path(sys.argv[1]).resolve()
output.parent.mkdir(parents=True, exist_ok=True)
assert not output.exists(), 'do not overwrite a reviewed render'
with tempfile.TemporaryDirectory(prefix='erlkonig-icon-') as directory:
    work = Path(directory)
    rust = '#[allow(dead_code)]\n#[path = ' + json.dumps(str(source)) + '] mod crown;\n' + r'''
fn main() {
    let frame = crown::parametric_frame(std::time::Duration::ZERO, None, false, false);
    assert!(!frame.animating);
    print!("[");
    for (i, point) in frame.points.iter().enumerate() {
        if i > 0 { print!(","); }
        print!("[{},{}]", point.x, point.y);
    }
    println!("]");
}
'''
    (work / 'points.rs').write_text(rust)
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2024', str(work/'points.rs'), '-o', str(work/'points')], check=True, timeout=30)
    points = subprocess.check_output([str(work/'points')], timeout=10)
    (work/'points.json').write_bytes(points)
    swift = r'''
import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers
let points = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))) as! [[Double]]
let size = 1024
let space = CGColorSpace(name: CGColorSpace.sRGB)!
let context = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
    bytesPerRow: size * 4, space: space,
    bitmapInfo: CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.noneSkipLast.rawValue)!
context.setFillColor(CGColor(colorSpace: space, components: [0,0,0,1])!)
context.fill(CGRect(x: 0, y: 0, width: size, height: size))
let xs = points.map { $0[0] }, ys = points.map { $0[1] }
let cx = (xs.min()! + xs.max()!) / 2, cy = (ys.min()! + ys.max()!) / 2
let scale = Double(size) * 0.64 / (xs.max()! - xs.min()!)
let path = CGMutablePath()
for (index, point) in points.enumerated() {
    let position = CGPoint(x: Double(size)/2 + (point[0]-cx)*scale,
                           y: Double(size)/2 - (point[1]-cy)*scale)
    if index == 0 { path.move(to: position) } else { path.addLine(to: position) }
}
path.closeSubpath()
context.addPath(path)
context.setFillColor(CGColor(colorSpace: space, components: [247.0/255,249.0/255,239.0/255,1])!)
context.fillPath()
let pixels = context.data!.assumingMemoryBound(to: UInt8.self)
func rgb(_ offset: Int) -> [UInt8] { [pixels[offset], pixels[offset+1], pixels[offset+2]] }
assert((0..<size*size).contains { rgb($0*4) == [247,249,239] })
for i in 0..<size {
    assert(rgb(i*4) == [0,0,0] && rgb(((size-1)*size+i)*4) == [0,0,0])
    assert(rgb(i*size*4) == [0,0,0] && rgb((i*size+size-1)*4) == [0,0,0])
}
let image = context.makeImage()!
let output = URL(fileURLWithPath: CommandLine.arguments[2])
let destination = CGImageDestinationCreateWithURL(output as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(destination, image, nil)
assert(CGImageDestinationFinalize(destination))
let png = try Data(contentsOf: output)
assert(png[25] == 2) // RGB PNG, no alpha channel; the OS supplies its icon mask.
print("1024x1024 opaque RGB; exact off-white interior, black perimeter; no baked mask")
'''
    (work/'render.swift').write_text(swift)
    subprocess.run(['/usr/bin/swift', str(work/'render.swift'), str(work/'points.json'), str(output)], check=True, timeout=90)
    receipt = {'geometry_source': 'native/crates/erlkonig-crown/src/lib.rs',
        'geometry_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
        'entry_point': 'parametric_frame(Duration::ZERO, None, false, false)',
        'points_sha256': hashlib.sha256(points).hexdigest(), 'points': len(json.loads(points)),
        'width': 1024, 'height': 1024, 'opaque_rgb': True, 'crown_srgb': '#f7f9ef',
        'background_srgb': '#000000', 'crown_width_fraction': 0.64, 'baked_corner_mask': False,
        'png_sha256': hashlib.sha256(output.read_bytes()).hexdigest()}
    output.with_suffix('.json').write_text(json.dumps(receipt, indent=2)+'\n')
    print(json.dumps(receipt))
