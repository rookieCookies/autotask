use std::{collections::VecDeque, sync::{RwLock, atomic::AtomicU8, Arc}, num::NonZeroUsize, thread::JoinHandle};

use once_cell::sync::Lazy;
pub use autotask_macros::task;

struct TaskQueue {
    ///
    /// 0 - Not picked up
    /// 1 - Not finished
    /// 2 - Finished
    ///
    queue: Vec<*mut (AtomicU8, *mut (dyn Task + 'static))>,
    finished: bool,
}

unsafe impl Sync for TaskQueue {}
unsafe impl Send for TaskQueue {}

pub struct Tasker {
    tasks: Arc<RwLock<TaskQueue>>,
    threads: Vec<JoinHandle<()>>
}


static TASKER : Lazy<Tasker> = Lazy::new(|| Tasker::new(NonZeroUsize::new(usize::MAX).unwrap()));


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
            tasks: Arc::new(RwLock::new(TaskQueue { queue: Vec::new(), finished: false })),
            threads: Vec::with_capacity(thread_count),
        };

        for _ in 0..thread_count {
            let task = slf.tasks.clone();
            slf.threads.push(std::thread::spawn(|| task_runner::<false>(task)));
        }

        slf
    }


    pub fn add_task<T: Task + 'static>(task: T) -> (*mut T, *mut (AtomicU8, *mut dyn Task)) {
        let task = Box::new(task);
        let task = Box::leak::<'static>(task);
        let alloc = Box::new((AtomicU8::new(0), task as *mut _));
        let alloc_ptr = Box::leak::<'static>(alloc);

        let task = task as *mut T;

        TASKER.tasks.write().unwrap().queue.push(alloc_ptr);
        (task, alloc_ptr)
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


pub struct TaskId(usize);


fn task_runner<const IS_FINITE: bool>(queue: Arc<RwLock<TaskQueue>>) {
    while !queue.read().unwrap().queue.is_empty() || (!queue.read().unwrap().finished && !IS_FINITE) {
        let Some(val) = queue.write().unwrap().queue.pop()
        else { continue };

        let (state, data) = unsafe { &*val };
        
        state.store(1, std::sync::atomic::Ordering::SeqCst);
        
        unsafe { &mut **data }.run();

        state.store(2, std::sync::atomic::Ordering::SeqCst);
    }
}
