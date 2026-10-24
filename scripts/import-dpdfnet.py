"""Regenerate the fixed, bounds-checked Rust DPDFNet-2 graph (development only).

Requires pycparser. Reads CEVA's Apache-2.0 generated graph as vendored by
HushMic; never compiles or executes C. Unknown syntax fails closed. The shipped
Rust source needs neither this script nor Python nor an ONNX/C runtime.
"""
import hashlib
from pathlib import Path
import re
import sys
from pycparser import c_ast as c, c_parser

ROOT = Path(__file__).resolve().parents[1]
SOURCE_SHA = 'ec77d6301fce90c8ae5de49e34990539b799331ff25a8158bd786559771dab07'
PROFILE = 'dpdfnet2_48khz_hr'


def parse(source):
    source=source.replace(b'\r\n',b'\n')
    assert hashlib.sha256(source).hexdigest() == SOURCE_SHA, 'Unrecognized graph revision'
    text = re.sub(r'/\*.*?\*/', '', source.decode().replace('\r\n','\n'), flags=re.S)
    text = re.sub(r'^#.*$', '', text, flags=re.M)
    prefix = 'typedef unsigned long size_t; typedef unsigned int uint32_t; typedef unsigned short uint16_t; typedef unsigned long uintptr_t;\n'
    for name in [PROFILE, 'dpdf_block', 'dpdf_dense', 'dpdf_convop', 'dpdf_native_model', 'dpdf_native_api_v1', 'dpdf_affine_fn', 'dpdf_axpy_fn', 'dpdf_transpose_fn']:
        prefix += f'typedef struct {name} {name};\n'
    return c_parser.CParser().parse(prefix + text)


class Rust:
    def __init__(self):
        self.floats = set()

    def floating(self, node):
        if isinstance(node, c.ArrayRef): return True
        if isinstance(node, c.Constant): return node.type in ('float', 'double')
        if isinstance(node, c.ID): return node.name in self.floats
        if isinstance(node, c.FuncCall): return True
        if isinstance(node, c.UnaryOp): return self.floating(node.expr)
        if isinstance(node, c.BinaryOp): return self.floating(node.left) or self.floating(node.right)
        if isinstance(node, c.TernaryOp): return self.floating(node.iftrue) or self.floating(node.iffalse)
        return False

    def expr(self, node, floating=False):
        if isinstance(node, c.ID): return node.name
        if isinstance(node, c.Constant):
            value = node.value.rstrip('fF')
            if node.type in ('float', 'double'): return value + 'f32'
            return value + '.0f32' if floating else value
        if isinstance(node, c.BinaryOp):
            use_float = self.floating(node)
            return f'({self.expr(node.left, use_float)} {node.op} {self.expr(node.right, use_float)})'
        if isinstance(node, c.UnaryOp):
            if node.op == 'sizeof': return '1'  # memcpy uses f32 elements, not bytes
            assert node.op in ('-', '+', '!'), node.op
            return f'({node.op}{self.expr(node.expr, floating)})'
        if isinstance(node, c.ArrayRef):
            base, offset = self.pointer(node.name)
            return f'{base}[({offset}) + ({self.expr(node.subscript)})]'
        if isinstance(node, c.TernaryOp):
            return f'(if {self.expr(node.cond)} {{ {self.expr(node.iftrue, self.floating(node))} }} else {{ {self.expr(node.iffalse, self.floating(node))} }})'
        if isinstance(node, c.FuncCall):
            methods = {'sqrtf': 'sqrt', 'logf': 'ln', 'expf': 'exp', 'tanhf': 'tanh'}
            assert node.name.name in methods, node.name.name
            return f'({self.expr(node.args.exprs[0], True)}).{methods[node.name.name]}()'
        if isinstance(node, c.Cast):
            assert node.to_type.type.type.names == ['float']
            return f'({self.expr(node.expr)} as f32)'
        raise ValueError(type(node).__name__)

    def pointer(self, node):
        if isinstance(node, c.ID):
            assert node.name in ('spec', 'spec_out', 'state_in', 'state_out', 'w')
            return node.name, '0'
        if isinstance(node, c.StructRef):
            assert node.name.name == 'm' and node.field.name in ('arena', 'weights')
            return ('a' if node.field.name == 'arena' else 'w'), '0'
        if isinstance(node, c.BinaryOp) and node.op == '+':
            base, offset = self.pointer(node.left)
            return base, f'({offset}+{self.expr(node.right)})'
        raise ValueError(f'Unsupported pointer: {node}')

    def layer_index(self, node):
        assert isinstance(node, c.ArrayRef)
        return self.expr(node.subscript)

    def call(self, node):
        args = node.args.exprs
        if isinstance(node.name, c.StructRef):
            assert node.name.field.name == 'transpose'
            x, src = self.pointer(args[0]); y, dst = self.pointer(args[1])
            assert x == y == 'a'
            return f'transpose(a, {src}, {dst}, {self.expr(args[2])}, {self.expr(args[3])});'
        name = node.name.name
        if name == 'memcpy':
            dst, d = self.pointer(args[0]); src, s = self.pointer(args[1]); n = self.expr(args[2])
            if dst == src: return f'{dst}.copy_within(({s})..({s})+({n}), {d});'
            return f'{dst}[({d})..({d})+({n})].copy_from_slice(&{src}[({s})..({s})+({n})]);'
        if name in ('dpdf_dense_run', 'dpdf_convop_run'):
            index = self.layer_index(args[0]); src, s = self.pointer(args[1]); dst, d = self.pointer(args[2])
            assert dst == 'a' and src in ('a', 'state_in')
            if name == 'dpdf_convop_run':
                assert src == 'a'
                return f'conv[{index}].run(a, {s}, {d});'
            rows = self.expr(args[3])
            if src == 'state_in': return f'dense[{index}].run_state(&state_in[({s})..], &mut a[({d})..], {rows});'
            return f'dense[{index}].run(a, {s}, {d}, {rows});'
        if name == 'dpdf_process_layout':
            index = self.layer_index(args[0]); src, s = self.pointer(args[1]); state, st = self.pointer(args[2]); dst, d = self.pointer(args[3]); out, o = self.pointer(args[4]); flags = self.expr(args[5])
            assert src == dst == out == 'a' and state == 'state_in'
            return f'{{ let b = &mut blocks[{index}]; let n = b.size(); b.run(&a[({s})..({s})+n], &state_in[({st})..({st})+n], {flags}); a[({d})..({d})+n].copy_from_slice(b.output()); a[({o})..({o})+n].copy_from_slice(b.state_out()); }}'
        raise ValueError(name)

    def stmt(self, node):
        if isinstance(node, c.Compound): return '{\n' + '\n'.join(self.stmt(v) for v in node.block_items or []) + '\n}'
        if isinstance(node, c.For):
            decl = node.init.decls[0]
            assert len(node.init.decls) == 1 and node.cond.op == '<' and node.next.op in ('p++', '++')
            return f'for {decl.name} in {self.expr(decl.init)}..{self.expr(node.cond.right)} {self.block(node.stmt)}'
        if isinstance(node, c.Assignment):
            return f'{self.expr(node.lvalue)} {node.op} {self.expr(node.rvalue, self.floating(node.lvalue))};'
        if isinstance(node, c.Decl):
            assert node.type.type.names == ['float']
            self.floats.add(node.name)
            return f'let mut {node.name}: f32 = {self.expr(node.init, True)};'
        if isinstance(node, c.FuncCall): return self.call(node)
        if isinstance(node, c.If):
            assert isinstance(node.cond, c.FuncCall) and node.cond.name.name == 'dpdf_process_layout'
            return self.call(node.cond)
        if isinstance(node, c.Return): return ''
        raise ValueError(type(node).__name__)

    def block(self, node):
        return self.stmt(node) if isinstance(node, c.Compound) else '{\n' + self.stmt(node) + '\n}'


def number(node):
    if isinstance(node, c.Constant): return node.value.rstrip('fF')
    if isinstance(node, c.BinaryOp) and node.op == '+': return str(int(number(node.left)) + int(number(node.right)))
    if isinstance(node, c.ID) and node.name == 'NULL': return None
    # A pointer to the unmodified input weight blob.
    if isinstance(node, c.ID) and node.name == 'w': return '0'
    raise ValueError(str(node))


def main():
    source = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / '.downloads/hushmic-reference/crates/hushmic-denoiser/native/artifacts/v1/dpdfnet2_48khz_hr/generated_model.c'
    tree = parse(source.read_bytes())
    funcs = {n.decl.name: n for n in tree.ext if isinstance(n, c.FuncDef)}
    init = funcs[PROFILE + '_init_state']
    seeds = next(n.init.exprs for n in init.body.block_items if isinstance(n, c.Decl) and n.name == 'seeds')
    create = funcs[PROFILE + '_create_config']
    dense, conv, blocks, copies = [], [], [], []
    class Visit(c.NodeVisitor):
        def visit_FuncCall(self, node):
            if not isinstance(node.name, c.ID): return
            name = node.name.name; args = node.args.exprs if node.args else []
            if name == 'memcpy' and isinstance(args[0], c.BinaryOp) and isinstance(args[0].left, c.StructRef):
                dst = number(args[0].right); src = number(args[1]); count = number(args[2].left)
                copies.append((dst, src, count))
            elif name == 'dpdf_dense_create':
                off, bias, k, n = [number(a) for a in args[:4]]
                dense.append((off, bias, k, n, number(args[5])))
            elif name == 'dpdf_convop_create':
                conv.append([number(a) for a in args[:15]])
            elif name == 'dpdf_create':
                blocks.append((number(args[0]), number(args[1]), number(args[3]), number(args[4])))
    Visit().visit(create)
    assert (len(dense), len(conv), len(blocks), len(copies)) == (219, 21, 4, 19)
    rust = Rust()
    body = funcs[PROFILE + '_process'].body.block_items[1:]  # drop null-pointer guard; Rust slices are checked
    optional = lambda v: 'None' if v is None else f'Some({v})'
    text = '// Generated from CEVA DPDFNet at 6a5dbd3; Apache-2.0. See NOTICE.md.\n'
    text += '// Regenerate with scripts/import-dpdfnet.py. Bounds checked; no C runtime.\n'
    text += '#![allow(clippy::all, unused_parens, unused_mut)]\nuse super::kernels::*;\n'
    text += 'pub(super) const INITIAL: &[f32] = &[\n' + ',\n'.join(rust.expr(s) for s in seeds) + '];\n'
    text += 'pub(super) const DENSE: &[(usize,Option<usize>,usize,usize,usize)] = &[\n'
    text += ',\n'.join(f'({o},{optional(b)},{k},{n},{t})' for o,b,k,n,t in dense) + '];\n'
    text += 'pub(super) const CONV: &[(usize,Option<usize>,[usize;13])] = &[\n'
    text += ',\n'.join(f'({v[0]},{optional(v[1])},[{",".join(v[2:])}])' for v in conv) + '];\n'
    text += 'pub(super) const BLOCKS: &[(usize,usize,f32,f32)] = &[\n'
    text += ',\n'.join(f'({f},{o},{a}f32,{b}f32)' for f,o,a,b in blocks) + '];\n'
    text += 'pub(super) fn constants(weights:&[f32])->Vec<f32>{ let mut w=vec![0.;7137];\n'
    text += '\n'.join(f'w[{d}..{int(d)+int(n)}].copy_from_slice(&weights[{s}..{int(s)+int(n)}]);' for d,s,n in copies) + '\nw\n}\n'
    text += 'impl super::Model {\npub(super) fn graph(&mut self,spec:&[f32;962],state_in:&[f32],spec_out:&mut[f32;962],state_out:&mut[f32]) {\n'
    text += 'let Self { arena:a, constants:w, dense, conv, blocks, .. } = self;\n'
    text += '\n'.join(rust.stmt(n) for n in body) + '\n}\n}\n'
    destination = ROOT / 'crates/dpdfnet/src/generated.rs'
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(text)
    print(f'Wrote {destination} ({len(text)} characters)')


if __name__ == '__main__': main()
