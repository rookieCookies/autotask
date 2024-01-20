use autotask::task;

fn main() {
    let a = dummy_task(69);
    let b = dummy_task(420);
    let c = dummy_task(30);

    println!("{}", a.get());
    println!("{}", b.get());
    println!("{}", c.get());
}

#[task]
fn dummy_task(goal: usize) -> usize {
    let mut curr = 0;
    loop {
        println!("{curr} {:?}", std::thread::current().id());
        if curr == goal { break curr }

        curr += 1;
    }
}
