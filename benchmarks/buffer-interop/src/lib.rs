//! OGM-22 M0 proof model. This is not a production ECMAScript Runtime.
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::{Cell, RefCell};
use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

const MAX_REENTRANCY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferError {
    InvalidAlignment,
    InvalidLength,
    AllocationFailed,
    OutOfBounds,
    Detached,
    ResizeExceedsCapacity,
    BorrowConflict,
    BorrowAlreadyActive,
    JsExecutionWhileBorrowed,
    LeaseActive,
    LeaseExpired,
    ReentrancyLimit,
}
impl fmt::Display for BufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for BufferError {}

/// Private fixed-capacity aligned allocation; its address never changes.
///
/// Safety invariants: Layout is valid/non-zero; this object uniquely owns the
/// pointer/Layout pair; Drop deallocates once; all slice ranges are checked;
/// mutable access is made only through an exclusive RefCell borrow.
pub struct OwnedMemory {
    ptr: NonNull<u8>,
    layout: Layout,
    capacity: usize,
}
impl OwnedMemory {
    pub fn zeroed(capacity: usize, alignment: usize) -> Result<Self, BufferError> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(BufferError::InvalidAlignment);
        }
        let layout = Layout::from_size_align(capacity.max(1), alignment)
            .map_err(|_| BufferError::InvalidAlignment)?;
        // SAFETY: validated non-zero Layout; allocation failure is checked.
        let ptr =
            NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or(BufferError::AllocationFailed)?;
        Ok(Self {
            ptr,
            layout,
            capacity,
        })
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    pub fn address(&self) -> usize {
        self.ptr.as_ptr() as usize
    }
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), BufferError> {
        let end = checked_end(offset, bytes.len())?;
        if end > self.capacity {
            return Err(BufferError::OutOfBounds);
        }
        // SAFETY: range is within allocation and &mut self is exclusive.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr.as_ptr().add(offset),
                bytes.len(),
            )
        };
        Ok(())
    }
    fn slice(&self, start: usize, end: usize) -> &[u8] {
        // SAFETY: caller validates the range; allocation outlives this borrow.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(start), end - start) }
    }
    fn slice_mut(&mut self, start: usize, end: usize) -> &mut [u8] {
        // SAFETY: caller validates the range; &mut self prevents aliasing.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(start), end - start) }
    }
}
impl Drop for OwnedMemory {
    fn drop(&mut self) {
        // SAFETY: unique owner retains the exact allocation pointer/Layout pair.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

#[derive(Clone)]
pub struct FinalizerProbe(Rc<Cell<usize>>);
impl FinalizerProbe {
    pub fn count(&self) -> usize {
        self.0.get()
    }
}
struct BackingStore {
    memory: OwnedMemory,
    len: usize,
    detached: bool,
    finalizer: FinalizerProbe,
}
impl Drop for BackingStore {
    fn drop(&mut self) {
        self.finalizer.0.set(self.finalizer.0.get() + 1);
    }
}

#[derive(Clone)]
pub struct ArrayBuffer {
    inner: Rc<RefCell<BackingStore>>,
}
impl ArrayBuffer {
    /// Moves allocation ownership into the backing store without copying.
    pub fn take_ownership(
        memory: OwnedMemory,
        len: usize,
    ) -> Result<(Self, FinalizerProbe), BufferError> {
        if len > memory.capacity() {
            return Err(BufferError::InvalidLength);
        }
        let finalizer = FinalizerProbe(Rc::new(Cell::new(0)));
        let store = BackingStore {
            memory,
            len,
            detached: false,
            finalizer: finalizer.clone(),
        };
        Ok((
            Self {
                inner: Rc::new(RefCell::new(store)),
            },
            finalizer,
        ))
    }
    /// Host owner and JS-visible buffer keep independent references to one allocation.
    pub fn external_owned(
        memory: OwnedMemory,
        len: usize,
    ) -> Result<(ExternalBufferOwner, Self, FinalizerProbe), BufferError> {
        let (buffer, finalizer) = Self::take_ownership(memory, len)?;
        let owner = ExternalBufferOwner {
            inner: buffer.inner.clone(),
        };
        Ok((owner, buffer, finalizer))
    }
    pub fn byte_len(&self) -> Result<usize, BufferError> {
        let s = self
            .inner
            .try_borrow()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        Ok(s.len)
    }
    pub fn is_detached(&self) -> Result<bool, BufferError> {
        Ok(self
            .inner
            .try_borrow()
            .map_err(|_| BufferError::BorrowConflict)?
            .detached)
    }
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), BufferError> {
        let s = self
            .inner
            .try_borrow()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        let end = checked_end(offset, out.len())?;
        if end > s.len {
            return Err(BufferError::OutOfBounds);
        }
        out.copy_from_slice(s.memory.slice(offset, end));
        Ok(())
    }
    pub fn write(&self, offset: usize, input: &[u8]) -> Result<(), BufferError> {
        let mut s = self
            .inner
            .try_borrow_mut()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        let end = checked_end(offset, input.len())?;
        if end > s.len {
            return Err(BufferError::OutOfBounds);
        }
        s.memory.slice_mut(offset, end).copy_from_slice(input);
        Ok(())
    }
    pub fn copy_to_vec(&self, offset: usize, len: usize) -> Result<Vec<u8>, BufferError> {
        let s = self
            .inner
            .try_borrow()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        let end = checked_end(offset, len)?;
        if end > s.len {
            return Err(BufferError::OutOfBounds);
        }
        Ok(s.memory.slice(offset, end).to_vec())
    }
    #[cfg(test)]
    fn address(&self) -> Result<usize, BufferError> {
        let s = self
            .inner
            .try_borrow()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        Ok(s.memory.address())
    }
}
pub struct ExternalBufferOwner {
    inner: Rc<RefCell<BackingStore>>,
}
impl ExternalBufferOwner {
    pub fn array_buffer(&self) -> ArrayBuffer {
        ArrayBuffer {
            inner: self.inner.clone(),
        }
    }
}

pub struct TypedArray {
    buffer: ArrayBuffer,
    offset: usize,
    len: usize,
}
impl Clone for TypedArray {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            offset: self.offset,
            len: self.len,
        }
    }
}
impl TypedArray {
    pub fn new(
        buffer: ArrayBuffer,
        offset: usize,
        count: usize,
        element_size: usize,
        alignment: usize,
    ) -> Result<Self, BufferError> {
        if element_size == 0
            || alignment == 0
            || !alignment.is_power_of_two()
            || !element_size.is_multiple_of(alignment)
        {
            return Err(BufferError::InvalidAlignment);
        }
        let len = count
            .checked_mul(element_size)
            .ok_or(BufferError::InvalidLength)?;
        let end = checked_end(offset, len)?;
        let s = buffer
            .inner
            .try_borrow()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        if end > s.len {
            return Err(BufferError::OutOfBounds);
        }
        let address = s
            .memory
            .address()
            .checked_add(offset)
            .ok_or(BufferError::InvalidAlignment)?;
        if !offset.is_multiple_of(alignment) || !address.is_multiple_of(alignment) {
            return Err(BufferError::InvalidAlignment);
        }
        drop(s);
        Ok(Self {
            buffer,
            offset,
            len,
        })
    }
    pub fn copy_to_vec(&self) -> Result<Vec<u8>, BufferError> {
        self.buffer.copy_to_vec(self.offset, self.len)
    }
}

pub struct LeasedArrayBuffer {
    buffer: ArrayBuffer,
    active: Rc<Cell<bool>>,
}
impl Clone for LeasedArrayBuffer {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            active: self.active.clone(),
        }
    }
}
impl LeasedArrayBuffer {
    fn check(&self) -> Result<(), BufferError> {
        if self.active.get() {
            Ok(())
        } else {
            Err(BufferError::LeaseExpired)
        }
    }
    pub fn byte_len(&self) -> Result<usize, BufferError> {
        self.check()?;
        self.buffer.byte_len()
    }
    pub fn read(&self, off: usize, out: &mut [u8]) -> Result<(), BufferError> {
        self.check()?;
        self.buffer.read(off, out)
    }
    pub fn write(&self, off: usize, input: &[u8]) -> Result<(), BufferError> {
        self.check()?;
        self.buffer.write(off, input)
    }
}
struct BorrowGuard<'a>(&'a Cell<usize>);
impl<'a> BorrowGuard<'a> {
    fn acquire(active: &'a Cell<usize>) -> Result<Self, BufferError> {
        if active.get() != 0 {
            return Err(BufferError::BorrowAlreadyActive);
        }
        active.set(1);
        Ok(Self(active))
    }
}
impl Drop for BorrowGuard<'_> {
    fn drop(&mut self) {
        self.0.set(0);
    }
}
struct LeaseGuard<'a> {
    count: &'a Cell<usize>,
    token: Rc<Cell<bool>>,
}
impl<'a> LeaseGuard<'a> {
    fn acquire(count: &'a Cell<usize>) -> Self {
        count.set(count.get() + 1);
        Self {
            count,
            token: Rc::new(Cell::new(true)),
        }
    }
}
impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        self.token.set(false);
        self.count.set(self.count.get() - 1);
    }
}
struct ExecutionGuard<'a>(&'a Cell<usize>);
impl Drop for ExecutionGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

/// Thread-affine proof Runtime; Rc/Cell make it !Send and !Sync.
pub struct Runtime {
    borrows: Cell<usize>,
    leases: Cell<usize>,
    depth: Cell<usize>,
    gc_count: Cell<usize>,
    _thread_affine: PhantomData<Rc<()>>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}
impl Runtime {
    pub fn new() -> Self {
        Self {
            borrows: Cell::new(0),
            leases: Cell::new(0),
            depth: Cell::new(0),
            gc_count: Cell::new(0),
            _thread_affine: PhantomData,
        }
    }
    pub fn execute_js<T>(&self, f: impl FnOnce() -> T) -> Result<T, BufferError> {
        if self.borrows.get() != 0 {
            return Err(BufferError::JsExecutionWhileBorrowed);
        }
        if self.depth.get() >= MAX_REENTRANCY {
            return Err(BufferError::ReentrancyLimit);
        }
        self.depth.set(self.depth.get() + 1);
        let _guard = ExecutionGuard(&self.depth);
        Ok(f())
    }
    pub fn with_scoped_read<T>(
        &self,
        view: &TypedArray,
        f: impl FnOnce(&[u8]) -> T,
    ) -> Result<T, BufferError> {
        let _guard = BorrowGuard::acquire(&self.borrows)?;
        let s = view
            .buffer
            .inner
            .try_borrow()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        let end = checked_end(view.offset, view.len)?;
        if end > s.len {
            return Err(BufferError::OutOfBounds);
        }
        Ok(f(s.memory.slice(view.offset, end)))
    }
    pub fn with_scoped_mut<T>(
        &self,
        view: &TypedArray,
        f: impl FnOnce(&mut [u8]) -> T,
    ) -> Result<T, BufferError> {
        let _guard = BorrowGuard::acquire(&self.borrows)?;
        let mut s = view
            .buffer
            .inner
            .try_borrow_mut()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        let end = checked_end(view.offset, view.len)?;
        if end > s.len {
            return Err(BufferError::OutOfBounds);
        }
        Ok(f(s.memory.slice_mut(view.offset, end)))
    }
    /// Cloned lease tokens are revoked on scope exit, including panic unwinding.
    pub fn with_scoped_view<T>(
        &self,
        buffer: &ArrayBuffer,
        f: impl FnOnce(&LeasedArrayBuffer) -> T,
    ) -> Result<T, BufferError> {
        if self.borrows.get() != 0 {
            return Err(BufferError::JsExecutionWhileBorrowed);
        }
        let _lease = LeaseGuard::acquire(&self.leases);
        let view = LeasedArrayBuffer {
            buffer: buffer.clone(),
            active: _lease.token.clone(),
        };
        Ok(f(&view))
    }
    pub fn resize(&self, buffer: &ArrayBuffer, new_len: usize) -> Result<(), BufferError> {
        self.ensure_mutation_allowed()?;
        let mut s = buffer
            .inner
            .try_borrow_mut()
            .map_err(|_| BufferError::BorrowConflict)?;
        ensure_attached(&s)?;
        if new_len > s.memory.capacity() {
            return Err(BufferError::ResizeExceedsCapacity);
        }
        let old = s.len;
        if new_len > old {
            s.memory.slice_mut(old, new_len).fill(0);
        } else if new_len < old {
            s.memory.slice_mut(new_len, old).fill(0);
        }
        s.len = new_len;
        Ok(())
    }
    pub fn detach(&self, buffer: &ArrayBuffer) -> Result<(), BufferError> {
        self.ensure_mutation_allowed()?;
        let mut s = buffer
            .inner
            .try_borrow_mut()
            .map_err(|_| BufferError::BorrowConflict)?;
        if s.detached {
            return Err(BufferError::Detached);
        }
        s.detached = true;
        s.len = 0;
        Ok(())
    }
    pub fn collect_garbage(&self) -> Result<usize, BufferError> {
        if self.borrows.get() != 0 {
            return Err(BufferError::JsExecutionWhileBorrowed);
        }
        if self.leases.get() != 0 {
            return Err(BufferError::LeaseActive);
        }
        self.gc_count.set(self.gc_count.get() + 1);
        Ok(self.gc_count.get())
    }
    fn ensure_mutation_allowed(&self) -> Result<(), BufferError> {
        if self.borrows.get() != 0 {
            return Err(BufferError::JsExecutionWhileBorrowed);
        }
        if self.leases.get() != 0 {
            return Err(BufferError::LeaseActive);
        }
        Ok(())
    }
}
fn ensure_attached(s: &BackingStore) -> Result<(), BufferError> {
    if s.detached {
        Err(BufferError::Detached)
    } else {
        Ok(())
    }
}
fn checked_end(offset: usize, len: usize) -> Result<usize, BufferError> {
    offset.checked_add(len).ok_or(BufferError::OutOfBounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    fn memory(bytes: &[u8], cap: usize, align: usize) -> OwnedMemory {
        let mut m = OwnedMemory::zeroed(cap, align).unwrap();
        m.write(0, bytes).unwrap();
        m
    }
    #[test]
    fn owned_transfer_address_and_finalizer() {
        let m = memory(&[1, 2, 3, 4], 16, 16);
        let address = m.address();
        let (b, f) = ArrayBuffer::take_ownership(m, 4).unwrap();
        let v = TypedArray::new(b.clone(), 0, 4, 1, 1).unwrap();
        assert_eq!(b.address().unwrap(), address);
        assert_eq!(v.copy_to_vec().unwrap(), [1, 2, 3, 4]);
        drop(v);
        drop(b);
        assert_eq!(f.count(), 1);
    }
    #[test]
    fn external_owner_drop_preserves_escaped_view() {
        let (owner, b, f) = ArrayBuffer::external_owned(memory(&[9, 8, 7, 6], 16, 8), 4).unwrap();
        let escaped = TypedArray::new(b.clone(), 0, 4, 1, 1).unwrap();
        drop(owner);
        drop(b);
        assert_eq!(f.count(), 0);
        assert_eq!(escaped.copy_to_vec().unwrap(), [9, 8, 7, 6]);
        drop(escaped);
        assert_eq!(f.count(), 1);
    }
    #[test]
    fn alignment_and_pointer_stability() {
        let (b, _) = ArrayBuffer::take_ownership(memory(&[1; 16], 64, 64), 16).unwrap();
        let ptr = b.address().unwrap();
        assert_eq!(
            TypedArray::new(b.clone(), 1, 1, 4, 4).err(),
            Some(BufferError::InvalidAlignment)
        );
        let v = TypedArray::new(b.clone(), 0, 4, 4, 4).unwrap();
        Runtime::new().resize(&b, 48).unwrap();
        assert_eq!(b.address().unwrap(), ptr);
        assert_eq!(v.copy_to_vec().unwrap(), [1; 16]);
    }
    #[test]
    fn typed_array_escape_retains_owned_storage() {
        let (b, f) = ArrayBuffer::take_ownership(memory(&[3, 4, 5, 6], 8, 8), 4).unwrap();
        let a = TypedArray::new(b.clone(), 0, 4, 1, 1).unwrap();
        let escaped = a.clone();
        drop(a);
        drop(b);
        assert_eq!(escaped.copy_to_vec().unwrap(), [3, 4, 5, 6]);
        assert_eq!(f.count(), 0);
        drop(escaped);
        assert_eq!(f.count(), 1);
    }
    #[test]
    fn resize_and_detach_invalidate_views() {
        let (b, _) = ArrayBuffer::take_ownership(memory(&[1, 2, 3, 4], 16, 8), 4).unwrap();
        let alias = b.clone();
        let fixed = TypedArray::new(b.clone(), 0, 4, 1, 1).unwrap();
        let rt = Runtime::new();
        rt.resize(&b, 8).unwrap();
        assert_eq!(b.copy_to_vec(4, 4).unwrap(), [0, 0, 0, 0]);
        rt.resize(&b, 2).unwrap();
        assert_eq!(fixed.copy_to_vec(), Err(BufferError::OutOfBounds));
        rt.detach(&b).unwrap();
        assert_eq!(alias.byte_len(), Err(BufferError::Detached));
        assert_eq!(rt.detach(&b), Err(BufferError::Detached));
    }
    #[test]
    fn scoped_lease_revokes_escape_and_blocks_mutation_gc() {
        let (b, _) = ArrayBuffer::take_ownership(memory(&[5, 6, 7, 8], 8, 8), 4).unwrap();
        let rt = Runtime::new();
        let escaped = rt
            .with_scoped_view(&b, |v| {
                assert_eq!(v.read(0, &mut [0; 4]), Ok(()));
                assert_eq!(rt.resize(&b, 8), Err(BufferError::LeaseActive));
                assert_eq!(rt.detach(&b), Err(BufferError::LeaseActive));
                assert_eq!(rt.collect_garbage(), Err(BufferError::LeaseActive));
                v.clone()
            })
            .unwrap();
        assert_eq!(escaped.byte_len(), Err(BufferError::LeaseExpired));
        assert_eq!(escaped.read(0, &mut [0; 1]), Err(BufferError::LeaseExpired));
    }
    #[test]
    fn js_and_gc_are_blocked_during_rust_slice_borrow() {
        let (b, _) = ArrayBuffer::take_ownership(memory(&[1, 2, 3, 4], 8, 8), 4).unwrap();
        let v = TypedArray::new(b, 0, 4, 1, 1).unwrap();
        let rt = Runtime::new();
        rt.with_scoped_mut(&v, |bytes| {
            bytes[0] = 42;
            assert_eq!(
                rt.execute_js(|| ()),
                Err(BufferError::JsExecutionWhileBorrowed)
            );
            assert_eq!(
                rt.collect_garbage(),
                Err(BufferError::JsExecutionWhileBorrowed)
            );
            assert_eq!(
                rt.with_scoped_read(&v, |_| ()),
                Err(BufferError::BorrowAlreadyActive)
            );
        })
        .unwrap();
        assert_eq!(v.copy_to_vec().unwrap(), [42, 2, 3, 4]);
        assert_eq!(rt.execute_js(|| 7).unwrap(), 7);
    }
    #[test]
    fn panic_and_callback_error_release_borrow_guard() {
        let (b, _) = ArrayBuffer::take_ownership(memory(&[1, 2, 3, 4], 8, 8), 4).unwrap();
        let v = TypedArray::new(b, 0, 4, 1, 1).unwrap();
        let rt = Runtime::new();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = rt.with_scoped_mut(&v, |_| panic!("host panic"));
        }));
        assert!(panic.is_err());
        assert_eq!(rt.execute_js(|| 11).unwrap(), 11);
        let error = rt.with_scoped_read(&v, |_| Err::<(), _>("callback error"));
        assert_eq!(error, Ok(Err("callback error")));
        assert_eq!(rt.collect_garbage(), Ok(1));
    }
    #[test]
    fn nested_js_execution_is_bounded() {
        let rt = Runtime::new();
        assert_eq!(rt.execute_js(|| rt.execute_js(|| 5).unwrap()).unwrap(), 5);
        fn recurse(rt: &Runtime, n: usize) -> Result<(), BufferError> {
            rt.execute_js(|| if n == 0 { Ok(()) } else { recurse(rt, n - 1) })?
        }
        assert_eq!(
            recurse(&rt, MAX_REENTRANCY),
            Err(BufferError::ReentrancyLimit)
        );
    }
    #[test]
    fn invalid_alignment_and_overflow_are_rejected() {
        assert_eq!(
            OwnedMemory::zeroed(4, 3).err(),
            Some(BufferError::InvalidAlignment)
        );
        let (b, _) = ArrayBuffer::take_ownership(memory(&[1, 2, 3, 4], 4, 4), 4).unwrap();
        assert_eq!(
            TypedArray::new(b, usize::MAX, 2, 4, 4).err(),
            Some(BufferError::OutOfBounds)
        );
    }
}
