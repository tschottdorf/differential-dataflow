//! A reference-counted wrapper sharing one owned trace.
//!
//! The types in this module, `TraceBox` and `TraceRc` and meant to parallel `RcBox` and `Rc` in `std::rc`.
//!
//! The first typee is an owned trace with some information about the cumulative requirements of the shared 
//! handles. This is roughly how much progress has each made, so we know which "read capabilities" they have
//! collectively dropped, and when it is safe to inform the trace of such progress.
//! 
//! The second type is a wrapper which presents as a `TraceReader`, but whose methods for advancing its read
//! capabilities interact with the `TraceBox` rather than directly with the owned trace. Ideally, instances 
//! `TraceRc` should appear indistinguishable from the underlying trace from a reading perspective, with the
//! exception that the trace may not compact its representation as fast as if it were exclusively owned.

use std::rc::Rc;
use std::cell::RefCell;

use timely::progress::frontier::MutableAntichain;

use lattice::Lattice;
use trace::TraceReader;
use trace::cursor::Cursor;

/// A wrapper around a trace which tracks the frontiers of all referees.
/// 
/// This is an internal type, unlikely to be useful to higher-level programs, but exposed just in case.
/// This type is equivalent to a `RefCell`, in that it wraps the mutable state that multiple referrers 
/// may influence.
pub struct TraceBox<K, V, T, R, Tr> 
where T: Lattice+Clone+'static, Tr: TraceReader<K,V,T,R> {
    phantom: ::std::marker::PhantomData<(K, V, R)>,
    /// accumulated holds on times for advancement.
    pub advance_frontiers: MutableAntichain<T>,
    /// accumulated holds on times for distinction.
    pub through_frontiers: MutableAntichain<T>,
    /// The wrapped trace.
    pub trace: Tr,
}

impl<K,V,T,R,Tr> TraceBox<K,V,T,R,Tr>
where T: Lattice+Clone+'static, Tr: TraceReader<K,V,T,R> {
    /// Moves an existing trace into a shareable trace wrapper.
    ///
    /// The trace may already exist and have non-initial advance and distinguish frontiers. The boxing
    /// process will fish these out and make sure that they are used for the initial read capabilities.
    pub fn new(mut trace: Tr) -> Self {

        let mut advance = MutableAntichain::new();
        for time in trace.advance_frontier() {
            advance.update(time, 1);
        }

        let mut through = MutableAntichain::new();
        for time in trace.distinguish_frontier() {
            through.update(time, 1);
        }

        TraceBox {
            phantom: ::std::marker::PhantomData,
            advance_frontiers: advance,
            through_frontiers: through,
            trace: trace,
        }
    }
    /// Replaces elements of `lower` with those of `upper`.
    pub fn adjust_advance_frontier(&mut self, lower: &[T], upper: &[T]) {
        for element in upper { self.advance_frontiers.update_and(element, 1, |_,_| {}); }
        for element in lower { self.advance_frontiers.update_and(element, -1, |_,_| {}); }
        self.trace.advance_by(self.advance_frontiers.elements());
    }
    /// Replaces elements of `lower` with those of `upper`.
    pub fn adjust_through_frontier(&mut self, lower: &[T], upper: &[T]) {
        for element in upper { self.through_frontiers.update_and(element, 1, |_,_| {}); }
        for element in lower { self.through_frontiers.update_and(element, -1, |_,_| {}); }
        self.trace.distinguish_since(self.through_frontiers.elements());
    }
}

/// A handle to a shared trace.
///
/// As long as the handle exists, the wrapped trace should continue to exist and will not advance its 
/// timestamps past the frontier maintained by the handle. The intent is that such a handle appears as
/// if it is a privately maintained trace, despite being backed by shared resources.
pub struct TraceRc<K,V,T,R,Tr> where T: Lattice+Clone+'static, Tr: TraceReader<K,V,T,R> {
    advance_frontier: Vec<T>,
    through_frontier: Vec<T>,
    /// Wrapped trace. Please be gentle when using.
    pub wrapper: Rc<RefCell<TraceBox<K,V,T,R,Tr>>>,
}

impl<K,V,T,R,Tr> TraceReader<K, V, T, R> for TraceRc<K,V,T,R,Tr> where T: Lattice+Clone+'static, Tr: TraceReader<K,V,T,R> {
    
    type Batch = Tr::Batch;
    type Cursor = Tr::Cursor;

    /// Sets frontier to now be elements in `frontier`.
    ///
    /// This change may not have immediately observable effects. It informs the shared trace that this 
    /// handle no longer requires access to times other than those in the future of `frontier`, but if
    /// there are other handles to the same trace, it may not yet be able to compact.
    fn advance_by(&mut self, frontier: &[T]) {
        self.wrapper.borrow_mut().adjust_advance_frontier(&self.advance_frontier[..], frontier);
        self.advance_frontier = frontier.to_vec();
    }
    fn advance_frontier(&mut self) -> &[T] { &self.advance_frontier[..] }
    /// Allows the trace to compact batches of times before `frontier`.
    fn distinguish_since(&mut self, frontier: &[T]) {
        self.wrapper.borrow_mut().adjust_through_frontier(&self.through_frontier[..], frontier);
        self.through_frontier = frontier.to_vec();        
    }
    fn distinguish_frontier(&mut self) -> &[T] { &self.through_frontier[..] }
    /// Creates a new cursor over the wrapped trace.
    fn cursor_through(&mut self, frontier: &[T]) -> Option<(Tr::Cursor, <Tr::Cursor as Cursor<K, V, T, R>>::Storage)> {
        ::std::cell::RefCell::borrow_mut(&self.wrapper).trace.cursor_through(frontier)
    }

    fn map_batches<F: FnMut(&Self::Batch)>(&mut self, f: F) {
        ::std::cell::RefCell::borrow_mut(&self.wrapper).trace.map_batches(f)
    }
}

impl<K,V,T,R,Tr> TraceRc<K,V,T,R,Tr> where T: Lattice+Clone+'static, Tr: TraceReader<K,V,T,R> {
    /// Allocates a new handle from an existing wrapped wrapper.
    pub fn make_from(trace: Tr) -> (Self, Rc<RefCell<TraceBox<K,V,T,R,Tr>>>) {

        let wrapped = Rc::new(RefCell::new(TraceBox::new(trace)));

        let handle = TraceRc {
            advance_frontier: wrapped.borrow().advance_frontiers.elements().to_vec(),
            through_frontier: wrapped.borrow().through_frontiers.elements().to_vec(),
            wrapper: wrapped.clone(),
        };

        (handle, wrapped)
    }
}

impl<K, V, T: Lattice+Clone, R, Tr> Clone for TraceRc<K, V, T, R, Tr> where Tr: TraceReader<K, V, T, R> {
    fn clone(&self) -> Self {
        // increase ref counts for this frontier
        self.wrapper.borrow_mut().adjust_advance_frontier(&[], &self.advance_frontier[..]);
        self.wrapper.borrow_mut().adjust_through_frontier(&[], &self.through_frontier[..]);
        TraceRc {
            advance_frontier: self.advance_frontier.clone(),
            through_frontier: self.through_frontier.clone(),
            wrapper: self.wrapper.clone(),
        }
    }
}

impl<K, V, T, R, Tr> Drop for TraceRc<K, V, T, R, Tr>
    where T: Lattice+Clone+'static, Tr: TraceReader<K, V, T, R> {
    fn drop(&mut self) {
        self.wrapper.borrow_mut().adjust_advance_frontier(&self.advance_frontier[..], &[]);
        self.wrapper.borrow_mut().adjust_through_frontier(&self.through_frontier[..], &[]);
        self.advance_frontier = Vec::new();
        self.through_frontier = Vec::new();
    }
}