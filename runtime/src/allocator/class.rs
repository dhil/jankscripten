use super::super::StrPtr;
use super::heap_values::*;
use super::{Heap, ALIGNMENT};
use std::marker::PhantomData;

/// A managed pointer to a `Class`
#[derive(Debug, PartialEq, Clone, Copy)]
#[repr(transparent)]
pub struct ClassPtr<'a> {
    ptr: *mut Class,
    _phantom: PhantomData<&'a ()>,
}

/// this describes the layout of the memory an ClassPtr points to
#[repr(C)]
struct Class {
    /// 1
    tag: Tag,
    /// 3
    offsets: Vec<(StrPtr, u32)>,
    /// 3
    transitions: Vec<(StrPtr, u32)>,
    /// 1 x n
    ///
    /// ```
    /// let offset = offsets.linear_search(field_name);
    /// class.fields.add(offset)
    /// ```
    fields: *mut Tag,
}

impl<'a> HeapPtr for ClassPtr<'a> {
    fn get_ptr(&self) -> *mut Tag {
        return self.ptr as *mut Tag;
    }

    fn get_data_size(&self, heap: &Heap) -> usize {
        let tag = unsafe { (*self.ptr).tag };
        let class_tag = tag.class_tag;
        let num_elements = heap.get_container_size(class_tag);
        return num_elements * ALIGNMENT;
    }
}

impl<'a> ClassPtr<'a> {
    /// This function is unsafe, because (1) we do not check that the class_tag
    /// is valid, and (2) we assume that `ptr` is valid.
    pub unsafe fn new(ptr: *mut Tag) -> Self {
        assert_eq!((*ptr).type_tag, TypeTag::Class);
        ClassPtr {
            ptr: ptr as *mut Class,
            _phantom: PhantomData,
        }
    }

    pub fn class_tag(&self) -> u16 {
        let tag = unsafe { (*self.ptr).tag };
        assert_eq!(tag.type_tag, TypeTag::Class);
        tag.class_tag
    }

    pub fn read_at(&self, heap: &'a Heap, index: usize) -> Option<AnyPtr<'a>> {
        let type_tag = self.class_tag();
        let len = heap.get_container_size(type_tag);
        assert!(index < len);
        let values = unsafe { &mut (*self.ptr).fields as *mut *mut Tag };
        let ptr = unsafe { *values.add(index) };

        if ptr.is_null() {
            None
        } else {
            Some(unsafe { AnyPtr::new(ptr) })
        }
    }

    pub fn write_at<P: HeapPtr>(&self, heap: &'a Heap, index: usize, value: P) {
        let type_tag = self.class_tag();
        let len = heap.get_container_size(type_tag);
        assert!(index < len);
        let values = unsafe { &mut (*self.ptr).fields as *mut *mut Tag };
        let ptr = unsafe { values.add(index) };
        unsafe {
            ptr.write(value.get_ptr());
        }
    }

    /// if name is found, write to it. if not, transition, clone, write, and
    /// update pointer
    /// TODO: updating this pointer in particular isn't enough. i think we
    /// have to have a double-pointer situation
    /// (ObjectPtr -> ArrayPtr -> [u8; n])
    #[allow(unused)] // remove after we extern
    pub fn insert<P: HeapPtr>(self, heap: &'a Heap, name: StrPtr, value: P) -> Option<Self> {
        let data = unsafe { &*self.ptr };
        let class_tag = data.tag.class_tag;
        let mut classes = heap.classes.borrow_mut();
        let class = classes.get_class(class_tag);
        match class.lookup(name) {
            Some(offset) => {
                self.write_at(heap, offset, value);
                Some(self)
            }
            None => {
                let size = class.size;
                drop(class);
                let new_tag = classes.transition(class_tag, name);
                drop(classes);
                let new_object = heap.alloc_container(new_tag)?;
                for i in 0..size {
                    if let Some(val) = self.read_at(heap, i) {
                        new_object.write_at(heap, i, val);
                    }
                }
                new_object.write_at(heap, size, value);
                Some(new_object)
            }
        }
    }
}
