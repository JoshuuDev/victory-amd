//! The shape of a finished module's control flow, where a consumer counts
//! on more than LLVM asks: one block that returns, and loops left towards
//! one block.
//!
//! Windows drivers take our DXIL as it is. On Linux it goes through
//! vkd3d-proton, whose structuriser (dxil-spirv) has to find SPIR-V's
//! merge and continue blocks in it, and has only met what the DXIL
//! compiler writes: one `ret`, and loops that an HLSL `break` leaves. The
//! lowering follows the console's code instead, which returns from inside
//! loops and leaves one loop towards several places. For some of those
//! dxil-spirv wrote a block that leaves a loop by neither its merge nor
//! its continue ("exits the loop ..., but not via a structured exit" in
//! spirv-val's words), and Mesa's RADV then faulted inside
//! `vkCreateComputePipelines`, which ends the process: 7 of the title's
//! first 100 compute programs.
//!
//! Both passes keep what the module computes: the same blocks run in the
//! same order with the same values, with only branches between them added.

use std::collections::HashMap;

#[cfg(test)]
use super::for_each_operand;
use super::opt::{Effect, effect, map_operands};
use super::{Inst, Module, Op, Term, Ty, TypeDef, V, pred, successors};

/// What the passes need to know of the blocks reached from the entry.
struct Flow {
    /// Reverse postorder.
    order: Vec<usize>,
    /// A block's place in `order`; `usize::MAX` if it is not reached.
    number: Vec<usize>,
    preds: Vec<Vec<usize>>,
    idom: Vec<usize>,
}

impl Flow {
    fn of(m: &Module) -> Flow {
        let n = m.blocks.len();
        let mut order = Vec::new();
        let mut number = vec![usize::MAX; n];
        if n != 0 {
            // Postorder without recursion: a block with the successors yet
            // to visit.
            let mut seen = vec![false; n];
            let mut stack = vec![(0usize, 0usize)];
            seen[0] = true;
            while let Some(&mut (b, ref mut next)) = stack.last_mut() {
                let succs = successors(m.blocks[b].term);
                if let Some(&s) = succs.get(*next) {
                    *next += 1;
                    if !seen[s] {
                        seen[s] = true;
                        stack.push((s, 0));
                    }
                } else {
                    order.push(b);
                    stack.pop();
                }
            }
            order.reverse();
        }
        for (k, &b) in order.iter().enumerate() {
            number[b] = k;
        }
        let mut preds = vec![Vec::new(); n];
        for &b in &order {
            for &s in successors(m.blocks[b].term).iter() {
                preds[s].push(b);
            }
        }
        // Immediate dominators (Cooper, Harvey and Kennedy's iteration).
        let mut idom = vec![usize::MAX; n];
        if n != 0 {
            idom[0] = 0;
        }
        let mut changed = true;
        while changed {
            changed = false;
            for &b in order.iter().skip(1) {
                let mut new = usize::MAX;
                for &p in &preds[b] {
                    if idom[p] == usize::MAX {
                        continue;
                    }
                    if new == usize::MAX {
                        new = p;
                        continue;
                    }
                    let (mut x, mut y) = (p, new);
                    while x != y {
                        while number[x] > number[y] {
                            x = idom[x];
                        }
                        while number[y] > number[x] {
                            y = idom[y];
                        }
                    }
                    new = x;
                }
                if idom[b] != new {
                    idom[b] = new;
                    changed = true;
                }
            }
        }
        Flow { order, number, preds, idom }
    }

    /// Whether every path from the entry to `b` passes `a` (both reached).
    fn dominates(&self, a: usize, mut b: usize) -> bool {
        while self.number[b] > self.number[a] {
            b = self.idom[b];
        }
        a == b
    }

    /// The loops, each as its header and the blocks of its body (the
    /// header among them), smallest first: an inner loop before the loops
    /// around it. Only loops entered through their header are found; the
    /// lowering makes no other.
    fn loops(&self) -> Vec<(usize, Vec<bool>)> {
        let n = self.number.len();
        let mut loops = Vec::new();
        for &h in &self.order {
            let latches: Vec<usize> = self.preds[h].iter().copied().filter(|&p| self.dominates(h, p)).collect();
            if latches.is_empty() {
                continue;
            }
            let mut body = vec![false; n];
            body[h] = true;
            let mut work = latches;
            while let Some(b) = work.pop() {
                if !body[b] {
                    body[b] = true;
                    work.extend(self.preds[b].iter().copied());
                }
            }
            loops.push((h, body));
        }
        loops.sort_by_key(|(h, body)| (body.iter().filter(|&&b| b).count(), *h));
        loops
    }
}

/// A loop being given one way out: its blocks, the ones that leave it, and
/// where each instruction of its blocks is made.
struct Way<'a> {
    flow: &'a Flow,
    body: &'a [bool],
    leaving: &'a [(usize, Term)],
    /// By instruction: its block if that is one of the loop's.
    made_in: Vec<usize>,
}

impl Way<'_> {
    /// The instruction `v` is, if it is made in the loop by a block that
    /// not every way out passes: behind the one way out it is no longer
    /// made on every path.
    fn lost(&self, v: V) -> Option<u32> {
        let V::Inst(i) = v else { return None };
        let made = self.made_in.get(i as usize).copied().filter(|&b| b != usize::MAX)?;
        (!self.leaving.iter().all(|&(b, _)| self.flow.dominates(made, b))).then_some(i)
    }
}

/// The instructions made again for one place behind a loop, in the order
/// they are to stand in, and which each stands for.
#[derive(Default)]
struct Again {
    made: Vec<u32>,
    of: HashMap<u32, V>,
}

impl Module {
    /// Every return through one block: a program that returns from several
    /// places gets a last block that alone returns, and the others branch
    /// to it. Whether anything changed.
    pub(crate) fn one_exit(&mut self) -> bool {
        let reached = self.reached();
        let returns: Vec<usize> = (0..self.blocks.len())
            .filter(|&b| reached[b] && matches!(self.blocks[b].term, Some(Term::Ret)))
            .collect();
        if returns.len() < 2 {
            return false;
        }
        let exit = self.new_block();
        for b in returns {
            self.terminate(b, Term::Br(exit));
        }
        self.terminate(exit, Term::Ret);
        true
    }

    /// Every loop left towards one block: where the blocks of a loop branch
    /// to several blocks outside it, they branch to a new block instead,
    /// with the number of the block they meant, and from there a chain of
    /// branches leads to each. Inner loops first, so that leaving two loops
    /// at once becomes leaving one and then the other. Whether anything
    /// changed.
    pub(crate) fn one_way_out_of_loops(&mut self) -> bool {
        let mut changed = false;
        // The loops that are left as they are ([`Module::join_ways_out`]),
        // by their headers.
        let mut left = Vec::new();
        // Each round gives one loop its one way out and looks again: the
        // new blocks belong to the loops around it.
        // (No more rounds than there were blocks: each loop has its turn once.)
        for _ in 0..self.blocks.len() {
            let flow = Flow::of(self);
            let found = flow.loops().into_iter().find_map(|(header, body)| {
                let targets = self.ways_out(&flow, &body);
                (targets.len() > 1 && !left.contains(&header)).then_some((header, body, targets))
            });
            let Some((header, body, targets)) = found else { break };
            if self.join_ways_out(&flow, &body, &targets) {
                changed = true;
            } else {
                left.push(header);
            }
        }
        changed
    }

    /// The blocks outside `body` that its blocks branch to, in the order
    /// the blocks are numbered.
    fn ways_out(&self, flow: &Flow, body: &[bool]) -> Vec<usize> {
        let mut targets = Vec::new();
        for &b in &flow.order {
            if !body[b] {
                continue;
            }
            for &s in successors(self.blocks[b].term).iter() {
                if !body[s] && !targets.contains(&s) {
                    targets.push(s);
                }
            }
        }
        targets.sort_unstable();
        targets
    }

    /// Gives the loop of `body` one way out. Not done, and `false`, where
    /// a value would have to be joined that cannot be: a join is for
    /// numbers. A resource handle, a call's record of results or an
    /// address must come straight from the instruction that makes it (the
    /// validator and the converters name a resource by that instruction),
    /// so such a value is made again where it is read if making it is free
    /// of effects, and the loop is left as it is if not.
    fn join_ways_out(&mut self, flow: &Flow, body: &[bool], targets: &[usize]) -> bool {
        let leaving: Vec<(usize, Term)> = flow
            .order
            .iter()
            .filter(|&&b| body[b])
            .filter_map(|&b| {
                let term = self.blocks[b].term?;
                successors(Some(term)).iter().any(|&s| !body[s]).then_some((b, term))
            })
            .collect();
        let mut made_in = vec![usize::MAX; self.insts.len()];
        for &b in flow.order.iter().filter(|&&b| body[b]) {
            for &i in self.blocks[b].phis.iter().chain(&self.blocks[b].body) {
                made_in[i as usize] = b;
            }
        }
        let way = Way { flow, body, leaving: &leaving, made_in };
        if !self.can_join(&way, targets) {
            return false;
        }
        let leaving = way.leaving;
        let i32t = self.int(32);
        let number_of = |t: usize| targets.iter().position(|&x| x == t);
        let out = self.new_block();
        // Which block was meant: its number, by the block that leaves.
        let mut meant = Vec::with_capacity(leaving.len());
        // Per target: by leaving block, whether it left towards it.
        let mut towards: Vec<Vec<bool>> = vec![vec![false; leaving.len()]; targets.len()];
        // Each block that leaves, by the blocks it left towards (one, or
        // the two sides of its branch).
        for (k, &(b, term)) in leaving.iter().enumerate() {
            let which = match term {
                Term::Br(t) => {
                    let n = number_of(t).expect("a way out");
                    towards[n][k] = true;
                    self.terminate(b, Term::Br(out));
                    self.i32(n as u32)
                }
                Term::CondBr(c, t, f) => match (number_of(t), number_of(f)) {
                    (Some(n), None) => {
                        towards[n][k] = true;
                        self.terminate(b, Term::CondBr(c, out, f));
                        self.i32(n as u32)
                    }
                    (None, Some(n)) => {
                        towards[n][k] = true;
                        self.terminate(b, Term::CondBr(c, t, out));
                        self.i32(n as u32)
                    }
                    (Some(x), Some(y)) => {
                        towards[x][k] = true;
                        towards[y][k] = true;
                        self.terminate(b, Term::Br(out));
                        let (x, y) = (self.i32(x as u32), self.i32(y as u32));
                        if x == y { x } else { self.select(b, c, x, y) }
                    }
                    (None, None) => unreachable!("a block that leaves"),
                },
                Term::Ret => unreachable!("a return is no branch"),
            };
            meant.push((which, b));
        }
        let which = self.phi(out, i32t);
        self.set_phi(which, meant);
        // The chain: the first block asks for target 0, the next for 1, and
        // the last one's other side is the last target. `from[n]` is the
        // block target `n` is now reached from.
        let mut from = Vec::with_capacity(targets.len());
        let mut at = out;
        for (n, &t) in targets.iter().enumerate().take(targets.len() - 1) {
            let k = self.i32(n as u32);
            let is = self.cmp(at, pred::EQ, which, k);
            from.push(at);
            if n + 2 == targets.len() {
                self.terminate(at, Term::CondBr(is, t, targets[n + 1]));
                from.push(at);
            } else {
                let next = self.new_block();
                self.terminate(at, Term::CondBr(is, t, next));
                at = next;
            }
        }
        // A target's joins took a value from each block that left towards
        // it: now one, joined in `out` from those blocks (and undefined
        // from the blocks that left towards another target).
        for (n, &t) in targets.iter().enumerate() {
            for p in self.blocks[t].phis.clone() {
                let Op::Phi(incoming) = self.insts[p as usize].op.clone() else { continue };
                let ty = self.insts[p as usize].ty;
                let (inside, mut rest): (Vec<_>, Vec<_>) = incoming.into_iter().partition(|&(_, b)| body[b]);
                if inside.is_empty() {
                    continue;
                }
                let value = self.joined_out(out, ty, leaving, |k, b| {
                    towards[n][k].then(|| inside.iter().find(|&&(_, from)| from == b).map(|&(v, _)| v)).flatten()
                });
                rest.push((value, from[n]));
                self.insts[p as usize].op = Op::Phi(rest);
            }
        }
        // A value made in the loop and read behind it was made on every
        // path to its reader. Paths now pass `out`, which the blocks that
        // left towards other targets reach too: where the value's block is
        // not on all of them, readers take it joined in `out` (a number),
        // or made again beside them (the rest).
        let mut joined: HashMap<u32, V> = HashMap::new();
        // The readers: the blocks the loop does not hold, the chain's among
        // them (they read only `which`, made in `out`).
        for b in flow.order.iter().copied().filter(|&b| !body[b]) {
            let mut again = Again::default();
            for i in self.blocks[b].body.clone() {
                let mut op = self.insts[i as usize].op.clone();
                map_operands(&mut op, |v| self.behind(&way, out, &mut joined, &mut again, v));
                self.insts[i as usize].op = op;
            }
            if let Some(Term::CondBr(c, t, f)) = self.blocks[b].term {
                let c = self.behind(&way, out, &mut joined, &mut again, c);
                self.terminate(b, Term::CondBr(c, t, f));
            }
            // What was made again stands before its readers.
            self.blocks[b].body.splice(0..0, again.made);
            // A join reads its value at the end of the block it comes from.
            for p in self.blocks[b].phis.clone() {
                let Op::Phi(mut incoming) = self.insts[p as usize].op.clone() else { continue };
                for (v, source) in &mut incoming {
                    // From a block that was there and is not the loop's:
                    // what `out` joined for a target comes from the chain
                    // and is already right.
                    if flow.number.get(*source).is_some_and(|&n| n != usize::MAX) && !body[*source] {
                        let mut again = Again::default();
                        *v = self.behind(&way, out, &mut joined, &mut again, *v);
                        self.blocks[*source].body.extend(again.made);
                    }
                }
                self.insts[p as usize].op = Op::Phi(incoming);
            }
        }
        true
    }

    /// Whether [`Module::join_ways_out`] can have every value it has to
    /// have behind the loop.
    fn can_join(&self, way: &Way, targets: &[usize]) -> bool {
        let (flow, body) = (way.flow, way.body);
        // A target's own joins of what the loop's blocks bring.
        for &t in targets {
            for &p in &self.blocks[t].phis {
                let Op::Phi(incoming) = &self.insts[p as usize].op else { continue };
                if incoming.iter().any(|&(_, b)| body[b]) && !self.is_number(self.insts[p as usize].ty) {
                    return false;
                }
            }
        }
        for b in flow.order.iter().copied().filter(|&b| !body[b]) {
            let mut can = true;
            for &i in &self.blocks[b].body {
                super::for_each_operand(&self.insts[i as usize].op, |v| can &= self.can_have_behind(way, v));
            }
            for &p in &self.blocks[b].phis {
                let Op::Phi(incoming) = &self.insts[p as usize].op else { continue };
                for &(v, source) in incoming {
                    if flow.number[source] != usize::MAX && !body[source] {
                        can &= self.can_have_behind(way, v);
                    }
                }
            }
            if let Some(Term::CondBr(c, ..)) = self.blocks[b].term {
                can &= self.can_have_behind(way, c);
            }
            if !can {
                return false;
            }
        }
        true
    }

    /// Whether `ty` is a type of numbers, whose values a join may choose
    /// between.
    fn is_number(&self, ty: Ty) -> bool {
        matches!(self.types[ty.0 as usize], TypeDef::Int(_) | TypeDef::Float | TypeDef::Double)
    }

    /// Whether instruction `i` gives the same value wherever it stands
    /// after its operands, and does nothing else.
    fn free_to_make_again(&self, i: u32) -> bool {
        match &self.insts[i as usize].op {
            Op::Call(f, _) => effect(&self.funcs[*f as usize]) == Effect::Pure,
            Op::Bin(..) | Op::Cast(..) | Op::Cmp(..) | Op::Select(..) | Op::Extract(..) | Op::Gep(..) => true,
            Op::Phi(_) | Op::Load(_) | Op::Store(..) | Op::AtomicRmw(..) | Op::CmpXchg(..) => false,
        }
    }

    /// Whether a reader behind the loop can have `v`: as it is, joined, or
    /// made again from operands it can have.
    fn can_have_behind(&self, way: &Way, v: V) -> bool {
        let Some(i) = way.lost(v) else { return true };
        if self.is_number(self.insts[i as usize].ty) {
            return true;
        }
        let mut can = self.free_to_make_again(i);
        if can {
            super::for_each_operand(&self.insts[i as usize].op, |o| can &= self.can_have_behind(way, o));
        }
        can
    }

    /// `v` for a reader behind the loop ([`Module::can_join`] said it can
    /// be had): `v` itself, its join in `out`, or the instruction made
    /// again, noted in `again` for the reader's block to take in.
    fn behind(&mut self, way: &Way, out: usize, joined: &mut HashMap<u32, V>, again: &mut Again, v: V) -> V {
        let Some(i) = way.lost(v) else { return v };
        let ty = self.insts[i as usize].ty;
        if self.is_number(ty) {
            if let Some(&join) = joined.get(&i) {
                return join;
            }
            let made = way.made_in[i as usize];
            let join = self.joined_out(out, ty, way.leaving, |_, b| way.flow.dominates(made, b).then_some(v));
            joined.insert(i, join);
            return join;
        }
        if let Some(&copy) = again.of.get(&i) {
            return copy;
        }
        let mut op = self.insts[i as usize].op.clone();
        map_operands(&mut op, |o| self.behind(way, out, joined, again, o));
        let copy = self.insts.len() as u32;
        self.insts.push(Inst { ty, op });
        if let Some(&tag) = self.tags.get(&i) {
            self.tags.insert(copy, tag);
        }
        // After its operands, which were made again before it.
        again.made.push(copy);
        again.of.insert(i, V::Inst(copy));
        V::Inst(copy)
    }

    /// A join in `out` of the values `of` gives for the blocks that leave
    /// (by their place in `leaving` and their number); undefined for the
    /// others.
    fn joined_out(
        &mut self,
        out: usize,
        ty: Ty,
        leaving: &[(usize, Term)],
        of: impl Fn(usize, usize) -> Option<V>,
    ) -> V {
        let undef = self.undef(ty);
        let incoming = leaving.iter().enumerate().map(|(k, &(b, _))| (of(k, b).unwrap_or(undef), b)).collect();
        let value = self.phi(out, ty);
        self.set_phi(value, incoming);
        value
    }
}

/// Whether every value is made before it is read on every path: what LLVM
/// asks of a module, and what moving branches about can break. For tests.
#[cfg(test)]
pub(super) fn well_formed(m: &Module) -> Result<(), String> {
    let flow = Flow::of(m);
    let mut place = HashMap::new();
    for &b in &flow.order {
        let block = &m.blocks[b];
        for (k, &i) in block.phis.iter().chain(&block.body).enumerate() {
            place.insert(i, (b, k));
        }
    }
    let ok = |v: V, b: usize, k: usize| -> bool {
        let V::Inst(i) = v else { return true };
        match place.get(&i) {
            Some(&(made, at)) => (made == b && at < k) || (made != b && flow.dominates(made, b)),
            None => false,
        }
    };
    for &b in &flow.order {
        let block = &m.blocks[b];
        for &p in &block.phis {
            let Op::Phi(incoming) = &m.insts[p as usize].op else { return Err(format!("block {b}: not a join")) };
            let mut sources: Vec<usize> = incoming.iter().map(|x| x.1).collect();
            let mut preds = flow.preds[b].clone();
            sources.sort_unstable();
            sources.dedup();
            preds.sort_unstable();
            preds.dedup();
            if sources != preds {
                return Err(format!("block {b}: a join from {sources:?}, reached from {preds:?}"));
            }
            for &(v, source) in incoming {
                if !ok(v, source, usize::MAX) {
                    return Err(format!("block {b}: a join reads {v:?} from {source}, not made there"));
                }
            }
        }
        for (k, &i) in block.body.iter().enumerate() {
            let mut bad = None;
            for_each_operand(&m.insts[i as usize].op, |v| {
                if !ok(v, b, block.phis.len() + k) {
                    bad = Some(v);
                }
            });
            if let Some(v) = bad {
                return Err(format!("block {b}: {v:?} read before it is made"));
            }
        }
        match block.term {
            Some(Term::CondBr(c, ..)) if !ok(c, b, usize::MAX) => {
                return Err(format!("block {b}: branches on {c:?}, not made there"));
            }
            None => return Err(format!("block {b}: no terminator")),
            _ => {}
        }
    }
    Ok(())
}

/// The loops left towards more than one block (their headers). For tests.
#[cfg(test)]
pub(super) fn loops_with_several_ways_out(m: &Module) -> Vec<usize> {
    let flow = Flow::of(m);
    flow.loops().into_iter().filter(|(_, body)| m.ways_out(&flow, body).len() > 1).map(|(h, _)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::super::opt::tests::{Kit, kit, run};
    use super::super::{Attr, Bin, Module, Op, Term, V, pred};
    use super::{Flow, loops_with_several_ways_out, well_formed};

    fn returns(m: &Module) -> usize {
        let reached = m.reached();
        (0..m.blocks.len()).filter(|&b| reached[b] && matches!(m.blocks[b].term, Some(Term::Ret))).count()
    }

    /// A loop that returns from inside and breaks to a return behind it:
    /// afterwards one block returns, the others reach it, and a module
    /// with one return is left as it is.
    #[test]
    fn several_returns_become_one() {
        let Kit { mut m, b, reads, .. } = kit();
        let (one, two) = (m.i32(1), m.i32(2));
        let (head, inside, behind, unreached, returns_inside) =
            (m.new_block(), m.new_block(), m.new_block(), m.new_block(), m.new_block());
        m.terminate(b, Term::Br(head));
        let c = m.call(head, reads, vec![one, two]);
        let leave = m.cmp(head, pred::EQ, c, one);
        m.terminate(head, Term::CondBr(leave, behind, inside));
        let d = m.call(inside, reads, vec![two, one]);
        let again = m.cmp(inside, pred::EQ, d, two);
        m.terminate(inside, Term::CondBr(again, head, returns_inside));
        m.terminate(returns_inside, Term::Ret);
        m.terminate(behind, Term::Ret);
        m.terminate(unreached, Term::Ret);
        let before = m.blocks.len();
        assert!(m.one_exit());
        assert_eq!(m.blocks.len(), before + 1);
        assert!(matches!(m.blocks[before].term, Some(Term::Ret)));
        for b in [returns_inside, behind] {
            assert!(matches!(m.blocks[b].term, Some(Term::Br(to)) if to == before));
        }
        // What nothing reaches is not led to the exit.
        assert!(matches!(m.blocks[unreached].term, Some(Term::Ret)));
        assert_eq!(returns(&m), 1);
        assert!(!m.one_exit());
        m.write();
    }

    /// A loop left towards three blocks, by a block that leaves both ways
    /// and by another; a value made in the loop is read behind it, directly
    /// and through a join. Afterwards the loop has one way out and the
    /// program stores what it stored.
    #[test]
    fn a_loop_left_three_ways_is_left_one_way() {
        let Kit { mut m, b, reads, writes, .. } = kit();
        let i32t = m.int(32);
        let k: Vec<V> = (0..8).map(|n| m.i32(n)).collect();
        let (head, mid, latch, a, bb, c, end) =
            (m.new_block(), m.new_block(), m.new_block(), m.new_block(), m.new_block(), m.new_block(), m.new_block());
        m.terminate(b, Term::Br(head));
        let count = m.phi(head, i32t);
        let x = m.call(head, reads, vec![k[1], k[0]]);
        let made = m.bin(head, Bin::Add, x, count);
        let leave = m.cmp(head, pred::EQ, x, k[1]);
        m.terminate(head, Term::CondBr(leave, a, mid));
        // Leaves both ways.
        let y = m.call(mid, reads, vec![k[2], k[0]]);
        let later = m.bin(mid, Bin::Xor, made, y);
        let on = m.cmp(mid, pred::EQ, y, k[0]);
        let which = m.cmp(mid, pred::EQ, y, k[1]);
        let two_ways = m.new_block();
        m.terminate(mid, Term::CondBr(on, latch, two_ways));
        m.terminate(two_ways, Term::CondBr(which, bb, c));
        let next = m.bin(latch, Bin::Add, count, k[1]);
        let more = m.cmp(latch, pred::IULT, next, k[3]);
        m.terminate(latch, Term::CondBr(more, head, a));
        m.set_phi(count, vec![(k[0], b), (next, latch)]);
        // `a` joins what the two blocks that reach it made; `bb` and `c`
        // read `later`, made on every path to them but not on every path
        // out of the loop.
        let joined = m.phi(a, i32t);
        m.set_phi(joined, vec![(made, head), (next, latch)]);
        m.call(a, writes, vec![k[4], joined]);
        m.call(a, writes, vec![k[5], made]);
        m.terminate(a, Term::Br(end));
        m.call(bb, writes, vec![k[6], later]);
        m.terminate(bb, Term::Br(end));
        m.call(c, writes, vec![k[7], later]);
        m.terminate(c, Term::Ret);
        let last = m.phi(end, i32t);
        m.set_phi(last, vec![(joined, a), (later, bb)]);
        m.call(end, writes, vec![k[0], last]);
        m.terminate(end, Term::Ret);
        well_formed(&m).unwrap();
        assert_eq!(loops_with_several_ways_out(&m), [head]);
        let inputs: Vec<Vec<u32>> = vec![vec![0; 4], vec![1, 0, 2, 5], vec![0, 1, 1, 1], vec![2, 2, 0, 0], vec![1, 1, 1, 1]];
        let before: Vec<_> = inputs.iter().map(|i| stored(&m, i)).collect();
        assert!(m.one_exit());
        assert!(m.one_way_out_of_loops());
        well_formed(&m).unwrap();
        assert!(loops_with_several_ways_out(&m).is_empty());
        assert_eq!(returns(&m), 1);
        let after: Vec<_> = inputs.iter().map(|i| stored(&m, i)).collect();
        assert_eq!(before, after);
        assert!(!m.one_way_out_of_loops());
        m.write();
    }

    /// A loop left two ways, whose second way reads a resource handle made
    /// in a block the first way does not pass. `free`: whether making the
    /// handle is free of effects.
    fn handle_read_behind(free: bool) -> (Module, [usize; 3], V) {
        let Kit { mut m, b, reads, writes, .. } = kit();
        let i32t = m.int(32);
        let i8t = m.int(8);
        let i8p = m.ptr(i8t);
        let handle = m.named_struct("dx.types.Handle", vec![i8p]);
        let make_ty = m.func_ty(handle, vec![i32t, i32t]);
        m.ptr(make_ty);
        let make = if free {
            m.function("dx.op.createHandleFromHeap", make_ty, Some(Attr::ReadNone), false)
        } else {
            m.function("dx.op.textureLoad.h", make_ty, Some(Attr::ReadOnly), false)
        };
        let read_ty = m.func_ty(i32t, vec![handle, i32t]);
        m.ptr(read_ty);
        let read = m.function("dx.op.bufferLoad.h", read_ty, Some(Attr::ReadOnly), false);
        let k: Vec<V> = (0..4).map(|n| m.i32(n)).collect();
        let (head, mid, latch, first, second) = (m.new_block(), m.new_block(), m.new_block(), m.new_block(), m.new_block());
        m.terminate(b, Term::Br(head));
        let x = m.call(head, reads, vec![k[1], k[0]]);
        let leave = m.cmp(head, pred::EQ, x, k[1]);
        m.terminate(head, Term::CondBr(leave, first, mid));
        let at = m.bin(mid, Bin::Add, x, k[2]);
        let h = m.call(mid, make, vec![at, k[0]]);
        let y = m.call(mid, read, vec![h, k[0]]);
        let on = m.cmp(mid, pred::EQ, y, k[0]);
        m.terminate(mid, Term::CondBr(on, latch, second));
        let more = m.cmp(latch, pred::IULT, y, k[3]);
        m.terminate(latch, Term::CondBr(more, head, first));
        m.call(first, writes, vec![k[0], x]);
        m.terminate(first, Term::Ret);
        let z = m.call(second, read, vec![h, k[1]]);
        m.call(second, writes, vec![k[1], z]);
        m.terminate(second, Term::Ret);
        well_formed(&m).unwrap();
        (m, [head, mid, second], h)
    }

    /// Whether some join of `m` is of something other than numbers.
    fn joins_of_other_than_numbers(m: &Module) -> bool {
        m.blocks.iter().flat_map(|b| &b.phis).any(|&p| !m.is_number(m.insts[p as usize].ty))
    }

    /// A handle is not joined: it is made again where it is read, from the
    /// number it was made from, which is joined.
    #[test]
    fn a_handle_read_behind_a_loop_is_made_again_there() {
        let (mut m, [head, _, second], h) = handle_read_behind(true);
        assert_eq!(loops_with_several_ways_out(&m), [head]);
        m.one_exit();
        assert!(m.one_way_out_of_loops());
        well_formed(&m).unwrap();
        assert!(loops_with_several_ways_out(&m).is_empty());
        assert!(!joins_of_other_than_numbers(&m));
        // The reader's first instruction is the handle's call again, with
        // the joined number; its read takes that one.
        let first = m.blocks[second].body[0];
        let V::Inst(was) = h else { unreachable!() };
        assert_ne!(first, was);
        let (Op::Call(f, args), Op::Call(g, old)) = (&m.insts[first as usize].op, &m.insts[was as usize].op) else {
            panic!("the handle's call")
        };
        assert_eq!(f, g);
        assert_ne!(args[0], old[0], "the number it is made from is the join");
        assert!(matches!(args[0], V::Inst(j) if matches!(m.insts[j as usize].op, Op::Phi(_))));
        let Op::Call(_, read) = &m.insts[m.blocks[second].body[1] as usize].op else { panic!("the read") };
        assert_eq!(read[0], V::Inst(first));
        m.write();
    }

    /// Where the value cannot be made again, the loop keeps its ways out
    /// and the module is as it was.
    #[test]
    fn a_loop_whose_value_cannot_be_had_behind_it_is_left() {
        let (mut m, [head, ..], _) = handle_read_behind(false);
        m.one_exit();
        let (blocks, insts) = (m.blocks.len(), m.insts.len());
        assert!(!m.one_way_out_of_loops());
        assert_eq!((m.blocks.len(), m.insts.len()), (blocks, insts));
        assert_eq!(loops_with_several_ways_out(&m), [head]);
        well_formed(&m).unwrap();
        m.write();
    }

    /// What `m` stores on `inputs` with the memory the loads read set from
    /// them first, so that runs take different ways.
    fn stored(m: &Module, inputs: &[u32]) -> Vec<(String, Vec<u64>)> {
        run(m, inputs)
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) % n
        }
    }

    /// A random program whose blocks branch any way forward, with branches
    /// back to blocks that dominate them: loops left from several blocks
    /// towards several, out of several at once, and returns anywhere.
    /// Values are read wherever their block dominates, and joined.
    fn random_flow(seed: u64) -> Module {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let Kit { mut m, b: entry, pure, reads, writes } = kit();
        let i32t = m.int(32);
        let n = 4 + rng.next(11) as usize;
        let blocks: Vec<usize> = std::iter::once(entry).chain((1..n).map(|_| m.new_block())).collect();
        assert!(blocks.iter().enumerate().all(|(k, &b)| k == b));
        let placeholder = m.bool(true);
        // Forward first.
        for b in 0..n {
            let ahead = |rng: &mut Rng| b + 1 + rng.next((n - b - 1) as u64) as usize;
            let term = if b == n - 1 || (b > 1 && rng.next(9) == 0) {
                Term::Ret
            } else if rng.next(3) == 0 {
                Term::Br(ahead(&mut rng))
            } else {
                Term::CondBr(placeholder, ahead(&mut rng), ahead(&mut rng))
            };
            m.terminate(b, term);
        }
        // Then back, to a dominator: dominance stays as it is.
        let forward = Flow::of(&m);
        let mut back = vec![None; n];
        for b in 1..n {
            if forward.number[b] == usize::MAX || rng.next(2) != 0 {
                continue;
            }
            let mut doms = Vec::new();
            let mut d = b;
            while d != 0 {
                doms.push(d);
                d = forward.idom[d];
            }
            let head = doms[rng.next(doms.len() as u64) as usize];
            match m.blocks[b].term {
                Some(Term::Br(t)) => {
                    m.terminate(b, Term::CondBr(placeholder, head, t));
                    back[b] = Some(head);
                }
                Some(Term::CondBr(_, _, f)) => {
                    m.terminate(b, Term::CondBr(placeholder, head, f));
                    back[b] = Some(head);
                }
                _ => {}
            }
        }
        let flow = Flow::of(&m);
        // Joins first, so that bodies can read them; their values last.
        let mut made: Vec<Vec<V>> = vec![Vec::new(); n];
        let mut joins: Vec<Vec<V>> = vec![Vec::new(); n];
        for &b in &flow.order {
            if flow.preds[b].len() > 1 {
                for _ in 0..1 + rng.next(2) {
                    let p = m.phi(b, i32t);
                    joins[b].push(p);
                    made[b].push(p);
                }
            }
        }
        // A value some block that dominates `b` made (or `b` itself, so far).
        let pick = |m: &mut Module, rng: &mut Rng, made: &[Vec<V>], b: usize| -> V {
            let mut from = Vec::new();
            let mut d = b;
            loop {
                from.extend(made[d].iter().copied());
                if d == 0 {
                    break;
                }
                d = flow.idom[d];
            }
            if from.is_empty() || rng.next(5) == 0 {
                let k = rng.next(4) as u32;
                m.i32(k)
            } else {
                from[rng.next(from.len() as u64) as usize]
            }
        };
        let slot = m.i32(15);
        let zero = m.i32(0);
        let one = m.i32(1);
        for &b in &flow.order {
            for _ in 0..1 + rng.next(4) {
                let (x, y) = (pick(&mut m, &mut rng, &made, b), pick(&mut m, &mut rng, &made, b));
                let v = match rng.next(4) {
                    0 => m.call(b, reads, vec![x, y]),
                    1 => m.call(b, pure, vec![x, y]),
                    2 => m.bin(b, Bin::Add, x, y),
                    _ => m.bin(b, Bin::Xor, x, y),
                };
                made[b].push(v);
            }
            let (at, v) = (rng.next(9) as u32, pick(&mut m, &mut rng, &made, b));
            let at = m.i32(at);
            m.call(b, writes, vec![at, v]);
            if let Some(Term::CondBr(_, t, f)) = m.blocks[b].term {
                let (x, y) = (pick(&mut m, &mut rng, &made, b), pick(&mut m, &mut rng, &made, b));
                let mut c = m.cmp(b, pred::EQ + rng.next(10) as u8, x, y);
                if back[b].is_some() {
                    // Branches back are counted, all in one place: a run ends.
                    let count = m.call(b, reads, vec![slot, zero]);
                    let next = m.bin(b, Bin::Add, count, one);
                    m.call(b, writes, vec![slot, next]);
                    let limit = m.i32(2 + rng.next(3) as u32);
                    let few = m.cmp(b, pred::IULT, count, limit);
                    c = m.bin(b, Bin::And, c, few);
                }
                m.terminate(b, Term::CondBr(c, t, f));
            }
        }
        for &b in &flow.order {
            for p in joins[b].clone() {
                let mut sources = flow.preds[b].clone();
                sources.sort_unstable();
                sources.dedup();
                let incoming = sources.into_iter().map(|s| (pick(&mut m, &mut rng, &made, s), s)).collect();
                m.set_phi(p, incoming);
            }
        }
        m
    }

    #[test]
    fn random_flows_store_the_same_with_one_way_out_of_each_loop() {
        let (mut several, mut ended) = (0, 0);
        for seed in 1..=6000u64 {
            let mut m = random_flow(seed);
            well_formed(&m).unwrap_or_else(|e| panic!("seed {seed}, as generated: {e}"));
            let inputs: Vec<Vec<u32>> =
                (0..5u32).map(|n| (0..7u32).map(|i| (seed as u32).wrapping_mul(n * 7 + i + 1) >> (n * 5)).collect()).collect();
            let before: Vec<_> = inputs.iter().map(|i| run(&m, i)).collect();
            let had = !loops_with_several_ways_out(&m).is_empty();
            several += usize::from(had);
            m.one_exit();
            let changed = m.one_way_out_of_loops();
            assert_eq!(changed, had, "seed {seed}");
            well_formed(&m).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            assert!(loops_with_several_ways_out(&m).is_empty(), "seed {seed}");
            assert!(returns(&m) <= 1, "seed {seed}");
            for (i, was) in inputs.iter().zip(&before) {
                // A run the interpreter gave up on took more blocks now.
                if was.last().is_some_and(|l| l.0 == "ret") {
                    ended += 1;
                    assert_eq!(&run(&m, i), was, "seed {seed}");
                }
            }
            m.write();
        }
        assert!(several > 250, "{several} programs had a loop left several ways");
        assert!(ended > 20_000, "{ended} runs ended");
    }
}
