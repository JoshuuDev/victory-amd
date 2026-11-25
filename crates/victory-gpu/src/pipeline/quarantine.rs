//! Pipelines whose making ends the process are found out and left out.
//!
//! Making a host pipeline hands a translated program to code that is not
//! ours: the driver's compiler on Windows; under Wine vkd3d-proton's
//! conversion to SPIR-V and then the Vulkan driver's compiler, in our
//! process. A fault there is not an error returned to us. Seen under Proton
//! with RADV: an access violation inside `vkCreateComputePipelines`
//! ("Exception 0xc0000005 in Unix call", which Wine cannot unwind from),
//! and a stack that overflowed in the conversion however large it was. The
//! process ends or hangs, the next run asks for the same pipeline at the
//! same place, and ends the same way: one such program kept the title from
//! ever passing that point.
//!
//! So a journal in the cache says which pipelines are being made (`+` when
//! one begins, `-` when it is done; `?` for a suspect not yet come to). A
//! run that finds pipelines begun and not done knows the last run ended
//! with them in the making: several, when several workers were at it, of
//! which at most one is to blame. Those are suspects, and a suspect is made
//! alone (`!` in the journal): while it is made no other pipeline is, so
//! that a run ending then ended in it. A pipeline begun alone and not done
//! is refused from then on (`refused.txt`, one line each, to be deleted by
//! whoever wants it tried again: after a driver update, say), and its
//! draws or dispatches are dropped like those of a program that does not
//! translate. A run that ends in order forgets what it had begun.
//!
//! What this cannot tell: a run ended by something else while a suspect is
//! being made alone (the user ending the process, a fault elsewhere)
//! refuses that pipeline wrongly, which the list's line undoes; and a
//! driver that compiles after the call has returned is not caught at all.
//!
//! A pipeline is known here by its shaders, not by its graphics state: what
//! faults is a program's code, and a pixel shader drawn with twenty states
//! is found out once.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::Shader;

const JOURNAL: &str = "making.txt";
const REFUSED: &str = "refused.txt";

/// The journal and the list of refused pipelines in `dir` (the cache's
/// folder of pipelines), for who watches a run from outside it.
pub(super) fn files(dir: &Path) -> [PathBuf; 2] {
    [dir.join(JOURNAL), dir.join(REFUSED)]
}

#[derive(Default)]
struct State {
    /// Where the journal and the list are; `None` until a cache is used
    /// (tests, tools that make no pipelines): nothing is kept then.
    dir: Option<PathBuf>,
    journal: Option<std::fs::File>,
    suspects: HashSet<String>,
    refused: HashSet<String>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// Held by every thread that makes a pipeline: together by those making
/// ordinary ones, alone by the one making a suspect. A suspect begun and
/// not done is to blame only if nothing else was in the making beside it.
static MAKERS: RwLock<()> = RwLock::new(());

fn state() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a pipeline of `shaders` is called in the journal.
fn name(shaders: &[Arc<Shader>]) -> String {
    let mut parts: Vec<String> = shaders.iter().map(|s| format!("{}:{}", s.stage, s.hash)).collect();
    parts.sort_unstable();
    parts.join("+")
}

/// What a journal says of the run that wrote it: the pipelines begun alone
/// and not done, and the others begun and not done.
fn unfinished(journal: &str) -> (Vec<String>, Vec<String>) {
    let mut open: Vec<(bool, &str)> = Vec::new();
    // Suspects the run had from the one before it and did not come to.
    let mut carried: Vec<&str> = Vec::new();
    // A line the run did not finish writing has no end, and is not read.
    for line in journal.split_inclusive('\n').filter_map(|l| l.strip_suffix('\n')) {
        let Some((mark, name)) = line.trim_end_matches('\r').split_once(' ') else { continue };
        match mark {
            "?" => carried.push(name),
            "+" => open.push((false, name)),
            "!" => {
                carried.retain(|&n| n != name);
                open.push((true, name));
            }
            "-" => {
                if let Some(at) = open.iter().rposition(|&(_, n)| n == name) {
                    open.remove(at);
                }
            }
            _ => {}
        }
    }
    let of = |alone: bool| {
        let carried = carried.iter().copied().filter(|_| !alone);
        let mut names: Vec<String> =
            open.iter().filter(|o| o.0 == alone).map(|o| o.1).chain(carried).map(str::to_owned).collect();
        names.sort_unstable();
        names.dedup();
        names
    };
    (of(true), of(false))
}

/// The names in a list of refused pipelines: one a line, `#` begins a
/// remark.
fn listed(list: &str) -> HashSet<String> {
    list.lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Reads what the last run left in `dir` and keeps this run's journal
/// there.
pub(super) fn open(dir: &Path) {
    let mut held = state();
    if held.is_some() {
        return;
    }
    let mut refused = listed(&std::fs::read_to_string(dir.join(REFUSED)).unwrap_or_default());
    let (alone, others) = unfinished(&std::fs::read_to_string(dir.join(JOURNAL)).unwrap_or_default());
    if !alone.is_empty() {
        let list = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(REFUSED));
        for name in &alone {
            tracing::error!(
                pipeline = name,
                list = %dir.join(REFUSED).display(),
                "the last run ended while this pipeline was being made alone: it is left out from now on \
                 (delete its line from the list to have it tried again)"
            );
            if refused.insert(name.clone())
                && let Ok(list) = &list
            {
                let mut list: &std::fs::File = list;
                let _ = writeln!(list, "{name}");
            }
        }
    }
    let suspects: HashSet<String> = others.into_iter().filter(|n| !refused.contains(n)).collect();
    if !suspects.is_empty() {
        tracing::warn!(
            pipelines = suspects.len(),
            "the last run ended with these pipelines being made: each is made alone this time"
        );
    }
    let _ = std::fs::create_dir_all(dir);
    // The suspects stay in the journal until they are done: a run that
    // ends before it reaches them leaves them suspects.
    let mut journal = std::fs::File::create(dir.join(JOURNAL)).ok();
    if let Some(journal) = &mut journal {
        let mut names: Vec<&String> = suspects.iter().collect();
        names.sort_unstable();
        for name in names {
            let _ = writeln!(journal, "? {name}");
        }
    }
    *held = Some(State { dir: Some(dir.to_owned()), journal, suspects, refused });
}

/// Whether a pipeline of `shaders` is refused for having ended a run.
///
/// A shader refused alone (`victory warm` makes each with stand-ins for the
/// other stages) is refused in every pipeline: the conversion that faults
/// is the shader's own.
pub(super) fn refused(shaders: &[Arc<Shader>]) -> bool {
    let held = state();
    let Some(state) = held.as_ref() else { return false };
    !state.refused.is_empty()
        && (state.refused.contains(&name(shaders))
            || shaders.len() > 1 && shaders.iter().any(|s| state.refused.contains(&name(std::slice::from_ref(s)))))
}

/// A pipeline in the making, from [`making`] until dropped.
pub(super) struct Making {
    name: Option<String>,
    _alone: Option<RwLockWriteGuard<'static, ()>>,
    _beside: Option<RwLockReadGuard<'static, ()>>,
}

/// Notes that a pipeline of `shaders` is being made from now until what is
/// returned is dropped. A suspect waits here until no other pipeline is in
/// the making, and the others wait while it is made.
pub(super) fn making(shaders: &[Arc<Shader>]) -> Making {
    let name = {
        let held = state();
        match held.as_ref() {
            Some(state) if state.journal.is_some() => name(shaders),
            // No cache in use, or the run is ending: nothing is noted.
            _ => return Making { name: None, _alone: None, _beside: None },
        }
    };
    let suspect = state().as_ref().is_some_and(|s| s.suspects.contains(&name));
    // Taken before the journal says so (and never with the state held):
    // the thread that holds it alone is the one the journal names.
    let (alone, beside) = if suspect {
        (Some(MAKERS.write().unwrap_or_else(|e| e.into_inner())), None)
    } else {
        (None, Some(MAKERS.read().unwrap_or_else(|e| e.into_inner())))
    };
    write(if suspect { '!' } else { '+' }, &name);
    Making { name: Some(name), _alone: alone, _beside: beside }
}

impl Drop for Making {
    fn drop(&mut self) {
        // A suspect stays one for the whole run: the same shaders with
        // another state are made alone as well, since it may be that state
        // the last run ended in. A run it gets through whole clears it.
        if let Some(name) = self.name.take() {
            write('-', &name);
        }
    }
}

fn write(mark: char, name: &str) {
    if let Some(state) = state().as_mut()
        && let Some(journal) = &mut state.journal
    {
        // One write per line, so that a line is whole or not there. Not
        // flushed to the disk: it is the process that ends, not the system.
        let _ = journal.write_all(format!("{mark} {name}\n").as_bytes());
    }
}

/// The run ends in order: what is being made now is not to blame for it,
/// and nothing is noted from here on (the workers may go on for the moment
/// the process still has).
pub(super) fn closing() {
    let mut held = state();
    if let Some(state) = held.as_mut()
        && let Some(dir) = &state.dir
    {
        // The journal is closed before it is written anew. Suspects this
        // run did not come to stay suspects.
        let came_to = state.journal.take().is_some();
        if came_to {
            let rest = unfinished(&std::fs::read_to_string(dir.join(JOURNAL)).unwrap_or_default());
            let carried: Vec<String> = rest.1.into_iter().filter(|n| state.suspects.contains(n)).collect();
            let rest: String = carried.iter().map(|n| format!("? {n}\n")).collect();
            let _ = std::fs::write(dir.join(JOURNAL), rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_names_what_was_begun_and_not_done() {
        let journal = "+ compute:aa\n+ pixel:bb+vertex:cc\n- compute:aa\n! compute:dd\n+ compute:ee\n- compute:ee\n+ comp";
        let (alone, others) = unfinished(journal);
        assert_eq!(alone, ["compute:dd"]);
        assert_eq!(others, ["pixel:bb+vertex:cc"]);
        // Begun twice, done once: still being made.
        let (alone, others) = unfinished("+ a:1\n+ a:1\n- a:1\n");
        assert!(alone.is_empty());
        assert_eq!(others, ["a:1"]);
        // A suspect carried over: still one until made alone, then done.
        assert_eq!(unfinished("? a:1\n? b:2\n! a:1\n- a:1\n"), (Vec::new(), vec!["b:2".to_owned()]));
        assert_eq!(unfinished("? a:1\n! a:1\n"), (vec!["a:1".to_owned()], Vec::new()));
        assert_eq!(unfinished(""), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_list_is_read_without_its_remarks() {
        let list = "# refused\ncompute:aa\n\n  pixel:bb+vertex:cc  # the lamp\n";
        let names = listed(list);
        assert_eq!(names.len(), 2);
        assert!(names.contains("compute:aa") && names.contains("pixel:bb+vertex:cc"));
    }

    fn shader(stage: &'static str, hash: &str) -> Arc<Shader> {
        Arc::new(Shader {
            stage,
            hash: hash.to_owned(),
            container: Vec::new(),
            translated: std::sync::OnceLock::new(),
        })
    }

    /// Tests share the one state: one at a time.
    static ONE: Mutex<()> = Mutex::new(());

    /// A suspect is made with nothing beside it, every time it is made in
    /// the run; an orderly end keeps only the suspects not come to, and
    /// notes nothing after it.
    #[test]
    fn a_suspect_is_made_alone_and_an_orderly_end_forgets_the_rest() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(JOURNAL), "+ compute:aa\n+ compute:bb\n").unwrap();
        *state() = None;
        open(dir.path());
        let journal = || std::fs::read_to_string(dir.path().join(JOURNAL)).unwrap();
        assert_eq!(journal(), "? compute:aa\n? compute:bb\n");
        let (suspect, other) = ([shader("compute", "aa")], [shader("compute", "cc")]);
        {
            let _ordinary = making(&other);
            // Another ordinary one goes beside it; the suspect would wait.
            let _beside = making(&[shader("compute", "dd")]);
            assert!(MAKERS.try_write().is_err());
        }
        {
            let _alone = making(&suspect);
            assert!(MAKERS.try_read().is_err(), "nothing is made beside a suspect");
        }
        // Still a suspect: made alone again.
        drop(making(&suspect));
        let begun = making(&other);
        assert_eq!(
            journal(),
            "? compute:aa\n? compute:bb\n+ compute:cc\n+ compute:dd\n- compute:dd\n- compute:cc\n\
             ! compute:aa\n- compute:aa\n! compute:aa\n- compute:aa\n+ compute:cc\n"
        );
        closing();
        assert_eq!(journal(), "? compute:bb\n");
        drop(begun);
        drop(making(&other));
        assert_eq!(journal(), "? compute:bb\n");
        assert!(!dir.path().join(REFUSED).exists());
        *state() = None;
    }

    /// Three runs over one folder: the first ends with two pipelines in the
    /// making, the second makes them alone and ends in the one to blame,
    /// the third refuses that one and makes the other as any.
    #[test]
    fn a_pipeline_that_ends_two_runs_is_refused() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let run = |journal_before: Option<&str>| -> State {
            if let Some(text) = journal_before {
                std::fs::write(dir.path().join(JOURNAL), text).unwrap();
            }
            *state() = None;
            open(dir.path());
            state().take().unwrap()
        };
        let first = run(Some("+ compute:good\n+ compute:bad\n+ compute:done\n- compute:done\n"));
        assert!(first.refused.is_empty());
        assert_eq!(first.suspects.len(), 2);
        drop(first);
        // The journal a second run leaves: the good one made alone, the bad
        // one begun alone.
        let second = run(Some("? compute:bad\n? compute:good\n! compute:good\n- compute:good\n! compute:bad\n+ compute:other\n"));
        assert_eq!(second.refused, HashSet::from(["compute:bad".to_owned()]));
        assert_eq!(second.suspects, HashSet::from(["compute:other".to_owned()]));
        drop(second);
        let third = run(None);
        assert!(third.refused.contains("compute:bad"));
        // Its own journal kept the suspect it had not come to.
        assert_eq!(third.suspects, HashSet::from(["compute:other".to_owned()]));
        assert_eq!(std::fs::read_to_string(dir.path().join(REFUSED)).unwrap(), "compute:bad\n");
    }
}
