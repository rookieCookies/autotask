#![feature(get_many_mut)]
use std::{sync::{RwLock, atomic::{AtomicU8, AtomicUsize}, Arc}, num::NonZeroUsize, thread::JoinHandle, mem::transmute };

use once_cell::sync::Lazy;
pub use autotask_macros::task;

struct TaskQueue {
    ///
    /// Format is:
    /// - padding necessary
    /// - vtable ptr
    /// - data (dyn Task)
    /// - padding necessary
    /// - len
    ///
    /// Rules for the atomic ptr
    /// - 0 is NOT CLAIMED
    /// - 1 is CLAIMED
    /// - 2 is FINISHED
    /// - 3 is CAN FREE
    ///
    queue: Vec<u8>,
    item_count: AtomicUsize,
    finished: bool,
}

unsafe impl Sync for TaskQueue {}
unsafe impl Send for TaskQueue {}

pub struct Tasker {
    tasks: Arc<RwLock<TaskQueue>>,
    threads: Vec<JoinHandle<()>>
}


static TASKER : Lazy<Tasker> = Lazy::new(|| Tasker::new(NonZeroUsize::new(4).unwrap()));


impl Tasker {
    pub fn new(max_thread_count: NonZeroUsize) -> Self {
        let thread_count = std::thread::available_parallelism();
        let thread_count : usize = match thread_count {
            Ok(v) => {
                let count : usize = v.into();
                count - 1
            },
            Err(_) => 0,
        };

        let thread_count = thread_count.min(max_thread_count.get());

        let mut slf = Self {
            tasks: Arc::new(RwLock::new(TaskQueue {
                queue: Vec::with_capacity(1024),
                finished: false,
                item_count: AtomicUsize::new(0),
            })),
            threads: Vec::with_capacity(thread_count),
        };

        for _ in 0..thread_count {
            let task = slf.tasks.clone();
            slf.threads.push(std::thread::spawn(|| task_runner::<false>(task)));
        }

        slf
    }


    pub fn add_task<T: Task + 'static>(task: T) -> (*const AtomicU8, *const T) {
        // Get tge data
        let vtable = get_vtable(&task);
        let task_data = unsafe { ::core::slice::from_raw_parts(
            (&task as *const T) as *const u8,
            ::core::mem::size_of::<T>(),
        ) };

        let queue_ptr = TASKER.tasks.read().unwrap().queue.as_ptr() as usize;

        // Calculate verything
        let value_align = core::mem::align_of::<T>();
        let value_size = core::mem::size_of::<T>();
        let mut value_padding = value_align - (queue_ptr % value_align);
        if value_padding == value_align { value_padding = 0; }
        let value_padding = value_padding;
        let size = value_padding + value_size + 8;

        let atomic_size  = core::mem::size_of::<AtomicU8>();
        let size = size + atomic_size;

        let ptr_align = core::mem::align_of::<*const u8>();
        let ptr_size = core::mem::size_of::<*const u8>();
        let mut ptr_padding = ptr_align - (size % ptr_align);
        if ptr_padding == ptr_align { ptr_padding = 0; }
        let ptr_padding = ptr_padding;
        let size = size + ptr_padding + ptr_size;

        let len_size = core::mem::size_of::<usize>();
        let size = size + len_size;
        // value padding
        let size = size + core::mem::size_of::<usize>();
        // ptr padding
        let size = size + core::mem::size_of::<usize>();

        // Write
        let mut lock = TASKER.tasks.write().unwrap();

        let queue = &mut lock.queue;

        let assert_len = queue.len();

        queue.reserve(size);

        queue.extend((0..value_padding).map(|_| 0));
        let value_ptr = unsafe { queue.as_ptr().add(queue.len()) };
        queue.extend_from_slice(task_data);

        // since atomicu8 has an align of 1, we don't need padding
        
        let atomic_ptr = unsafe { queue.as_ptr().add(queue.len()) };
        queue.push(0u8);

        queue.extend((0..ptr_padding).map(|_| 0));
        queue.extend_from_slice(&(vtable as usize).to_ne_bytes());

        // we can assume, since these are all usizes, that there's no padding
        // necessary between the pointer and the usize
        
        // ptr padding
        queue.extend_from_slice(&ptr_padding.to_ne_bytes());

        queue.extend_from_slice(&value_align.to_ne_bytes());
        queue.extend_from_slice(&value_size.to_ne_bytes());

        queue.extend_from_slice(&size.to_ne_bytes());

        assert_eq!(queue.len(), assert_len + size);

        lock.item_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        drop(lock);

        (atomic_ptr.cast(), value_ptr.cast())
    }


    pub fn exhaust() {
        task_runner::<true>(TASKER.tasks.clone());
    }
}


impl Drop for Tasker {
    fn drop(&mut self) {
        self.tasks.write().unwrap().finished = true;
        Tasker::exhaust();

        std::mem::take(&mut self.threads).into_iter().for_each(|t| t.join().unwrap());
        assert!(self.tasks.read().unwrap().queue.is_empty());
    }
}


pub trait Task {
    fn run(&mut self);
}


pub enum TaskStatus {
    Complete,
    Errored,
    Processing,
}


fn task_runner<const IS_FINITE: bool>(queue: Arc<RwLock<TaskQueue>>) {
    let mut current_data = Vec::from([0]);
    'l: loop {
        let lock = queue.read().unwrap();
        
        if lock.queue.is_empty() && IS_FINITE {
            break
        }

        if lock.queue.is_empty() && lock.finished {
            break
        }

        if lock.item_count.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            if lock.finished || IS_FINITE { break }
            continue
        }

        if lock.queue.is_empty() {
            continue
        }

        drop(lock);

        current_data.clear();

        let mut lock = queue.write().unwrap();
        let ptr = (&mut lock.queue) as *mut Vec<u8>;

        let (state, val) = {
            let mut main_queue = unsafe { &mut **ptr };
            // Find which task is free to claim
            loop {
                let queue = &mut main_queue;

                if queue.is_empty() && !IS_FINITE { continue 'l }
                if queue.is_empty() && IS_FINITE { break 'l }

                let len = queue.len() - core::mem::size_of::<usize>();
                let total_len = &mut queue[len..];
                let total_len = usize::from_ne_bytes(total_len.try_into().unwrap());
                let len = queue.len() - core::mem::size_of::<usize>();
                let queue = &mut queue[..len];

                // Don't care about the data len or align
                let len = queue.len() - core::mem::size_of::<usize>();
                let queue = &mut queue[..len];
                let len = queue.len() - core::mem::size_of::<usize>();
                let queue = &mut queue[..len];

                let len = queue.len() - core::mem::size_of::<usize>();
                let ptr_padding = &mut queue[len..];
                let ptr_padding = usize::from_ne_bytes(ptr_padding.try_into().unwrap());
                let len = queue.len() - core::mem::size_of::<usize>();
                let queue = &mut queue[..len];

                // Don't care about the vtable pointer
                let len = queue.len() - core::mem::size_of::<*const u8>();
                let queue = &mut queue[..len];

                let len = queue.len() - ptr_padding;
                let queue = &mut queue[..len];

                let len = queue.len() - 1;
                let atomic_u8 = queue.get_mut(len).unwrap();
                let atomic_u8 = atomic_u8 as *mut u8 as *mut AtomicU8;

                if unsafe { &mut *atomic_u8 }.compare_exchange(0, 1, std::sync::atomic::Ordering::Acquire,
                                                           std::sync::atomic::Ordering::Relaxed).is_err() {
                    lock.item_count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    let len = main_queue.len() - total_len;
                    main_queue = &mut main_queue[..len];
                } else { break };
            }

            let queue = main_queue;

            // We don't care about the total length
            let len = queue.len() - core::mem::size_of::<usize>();
            let queue = &queue[..len];

            let data_len = &queue[queue.len() - core::mem::size_of::<usize>()..];
            let data_len = usize::from_ne_bytes(data_len.try_into().unwrap());
            let len = queue.len() - core::mem::size_of::<usize>();
            let queue = &queue[..len];

            let data_align = &queue[queue.len() - core::mem::size_of::<usize>()..];
            let data_align = usize::from_ne_bytes(data_align.try_into().unwrap());
            let len = queue.len() - core::mem::size_of::<usize>();
            let queue = &queue[..len];

            let ptr_padding = &queue[queue.len() - core::mem::size_of::<usize>()..];
            let ptr_padding = usize::from_ne_bytes(ptr_padding.try_into().unwrap());
            let len = queue.len() - core::mem::size_of::<usize>();
            let queue = &queue[..len];

            let vtable = &queue[queue.len() - core::mem::size_of::<*const u8>()..];
            let vtable = usize::from_ne_bytes(vtable.try_into().unwrap());
            let vtable = vtable as *const u8;
            let len = queue.len() - core::mem::size_of::<*const u8>();
            let queue = &queue[..len];

            let len = queue.len() - ptr_padding;
            let queue = &queue[..len];

            let len = queue.len() - 1;
            let ptr = queue.as_ptr();
            let atomic_u8 = len;

            let len = queue.len() - data_len - 1;
            let data = unsafe { ptr.add(len) };
            let data = {
                current_data.reserve(data_len + data_align);
                let ptr = current_data.as_ptr() as usize;
                let padding = data_align - (ptr % data_align);

                current_data.extend((0..padding).map(|_| 0));

                let len = current_data.len();
                current_data.extend((0..data_len).map(|i| unsafe { *data.add(i) }));

                unsafe { current_data.as_ptr().add(len) }
            };

            let data = unsafe { transmute::<(*const u8, *const u8), *const dyn Task>((data, vtable)) }; 

            (atomic_u8, data)
        };

        drop(lock);

        unsafe { &mut *val.cast_mut() }.run();

        {
            let mut lock = queue.write().unwrap();
            let queue = lock.queue.get_mut(state).unwrap();
            let value = unsafe { transmute::<&mut u8, &mut AtomicU8>(queue) };
            value.store(2, core::sync::atomic::Ordering::SeqCst);
        }

        // Clean up after ourselves

        let lock = queue.read().unwrap();
        let mut main_queue = &*lock.queue;
        let mut cleanup_len = 0;

        loop {
            let queue = main_queue;

            if queue.is_empty() { break }

            let total_len = &queue[queue.len() - core::mem::size_of::<usize>()..];
            let total_len = usize::from_ne_bytes(total_len.try_into().unwrap());
            let queue = &queue[..queue.len() - core::mem::size_of::<usize>()];

            // Don't care about the data len or align
            let queue = &queue[..queue.len() - core::mem::size_of::<usize>()];
            let queue = &queue[..queue.len() - core::mem::size_of::<usize>()];

            let ptr_padding = &queue[queue.len() - core::mem::size_of::<usize>()..];
            let ptr_padding = usize::from_ne_bytes(ptr_padding.try_into().unwrap());
            let queue = &queue[..queue.len() - core::mem::size_of::<usize>()];

            // Don't care about the vtable pointer
            let queue = &queue[..queue.len() - core::mem::size_of::<*const u8>()];

            let queue = &queue[..queue.len() - ptr_padding];

            let len = queue.len() - 1;
            let atomic_u8 = unsafe { queue.as_ptr().add(len) };
            let atomic_u8 = atomic_u8 as *const AtomicU8;

            if unsafe { atomic_u8.read() }.load(std::sync::atomic::Ordering::Acquire) == 3 {
                cleanup_len += total_len;
                main_queue = &main_queue[..main_queue.len() - total_len];

            } else { break };
        }

        drop(lock);

        if cleanup_len > 0 {
            let queue = &mut queue.write().unwrap().queue;
            let queue_len = queue.len();
            queue.truncate(queue_len - cleanup_len);
        }
    }
}


fn get_vtable(val: &dyn Task) -> *const usize {
    let (_, vtable) = unsafe { core::mem::transmute_copy::<_, (*const u8, *const usize)>(&val) };
    vtable
}
