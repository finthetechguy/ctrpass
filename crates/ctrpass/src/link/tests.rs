use super::*;
use crate::cec::container::tests::sample;
use crate::cec::envelope;
use crate::sim::{Behaviour, Console, KEYS};

const US: Mac = [0x9a, 0x68, 0x0a, 1, 2, 3];
const T1: u32 = 0x0002_0800;
const T2: u32 = 0x0003_0600;
const T3: u32 = 0x0008_c500;

struct TestHost {
    plan: Option<Plan>,
    refuse: Vec<u32>,
    offers: Vec<Vec<Entry>>,
}

impl Host for TestHost {
    fn choose(&mut self, _: Mac, _: &[Service]) -> Option<Plan> {
        self.plan.clone()
    }

    fn admit(&mut self, offer: &[Entry]) -> BTreeMap<u32, u16> {
        self.offers.push(offer.to_vec());
        offer
            .iter()
            .map(|e| {
                let code = if self.refuse.contains(&e.title) {
                    object::VERDICT_REFUSE
                } else {
                    object::VERDICT_ADMIT
                };
                (e.title, code)
            })
            .collect()
    }
}

fn services(titles: &[u32]) -> Vec<Service> {
    titles
        .iter()
        .map(|&t| Service::new(t, 0x30, vec![]).unwrap())
        .collect()
}

fn host(refuse: Vec<u32>) -> TestHost {
    TestHost {
        plan: Some(Plan {
            services: services(&[T1, T2, T3]),
            messages: vec![sample(T1, 1), sample(T2, 2)],
        }),
        refuse,
        offers: Vec::new(),
    }
}

fn console(behaviour: Behaviour, now: Instant) -> Console {
    Console::new(behaviour, &[sample(T3, 3)], services(&[T1, T2, T3]), now)
}

fn run(
    role: Role,
    console: &mut Console,
    host: &mut TestHost,
    loss: u32,
    seed: u64,
) -> (Link, Vec<Event>) {
    let config = Config {
        role,
        mac: US,
        token: [9; 8],
        services: services(&[T1, T2, T3]),
        channels: vec![1],
        dwell: DWELL,
        linger: LINGER,
    };
    let mut link = Link::new(config, &Keys::parse(KEYS).unwrap(), Instant::now());
    let events = crate::sim::run(&mut link, console, host, loss, seed);
    (link, events)
}

fn envelope_titles(objects: &[Vec<u8>]) -> Vec<u32> {
    objects.iter().filter_map(|o| envelope::title(o)).collect()
}

fn check_exchange(link: &Link, console: &Console, host: &TestHost) {
    assert!(link.is_done());
    let out = link.outcome().unwrap();
    assert!(out.peer_sent_messages);
    assert_eq!(envelope_titles(&out.delivered), vec![T1]);
    assert_eq!(envelope_titles(&out.received), vec![T3]);
    assert_eq!(out.refused, vec![T2]);
    let admitted = out.admitted.unwrap();
    assert_eq!(
        admitted
            .iter()
            .map(|e| (e.title, e.code))
            .collect::<Vec<_>>(),
        vec![(T3, object::VERDICT_ADMIT)]
    );
    assert_eq!(envelope_titles(console.received()), vec![T1]);
    assert_eq!(host.offers.len(), 1);
}

#[test]
fn lead_exchange() {
    let now = Instant::now();
    let mut c = console(Behaviour::Hosts, now);
    c.refuse = vec![T2];
    let mut h = host(vec![]);
    let (link, events) = run(Role::Lead, &mut c, &mut h, 0, 0);
    check_exchange(&link, &c, &h);
    let kinds: Vec<_> = c.received().iter().map(|o| object::kind_of(o)).collect();
    assert_eq!(
        kinds,
        vec![
            Some(0x41),
            Some(0x43),
            Some(0x11),
            Some(0x21),
            Some(0x23),
            Some(0x31),
            Some(0x33),
            Some(0x81),
            None
        ]
    );
    assert!(events.contains(&Event::LinkUp { peer: c.mac }));
}

#[test]
fn wait_exchange() {
    let now = Instant::now();
    let mut c = console(Behaviour::Leads, now);
    c.refuse = vec![T2];
    let mut h = host(vec![]);
    let (link, _) = run(Role::Wait, &mut c, &mut h, 0, 0);
    check_exchange(&link, &c, &h);
    let kinds: Vec<_> = c.received().iter().map(|o| object::kind_of(o)).collect();
    assert_eq!(
        kinds,
        vec![
            Some(0x42),
            Some(0x43),
            Some(0x12),
            Some(0x22),
            Some(0x24),
            Some(0x32),
            Some(0x34),
            Some(0x81),
            None
        ]
    );
}

#[test]
fn local_refusal_reaches_console() {
    for (role, behaviour) in [
        (Role::Lead, Behaviour::Hosts),
        (Role::Wait, Behaviour::Leads),
    ] {
        let now = Instant::now();
        let mut c = console(behaviour, now);
        let mut h = host(vec![T3]);
        let (link, _) = run(role, &mut c, &mut h, 0, 0);
        let out = link.outcome().unwrap();
        assert!(envelope_titles(&out.received).is_empty(), "{role:?}");
        assert_eq!(envelope_titles(c.received()), vec![T1, T2], "{role:?}");
    }
}

#[test]
fn exchange_survives_loss() {
    for (role, behaviour) in [
        (Role::Lead, Behaviour::Hosts),
        (Role::Wait, Behaviour::Leads),
    ] {
        let mut done = 0;
        for seed in 0..20 {
            let now = Instant::now();
            let mut c = console(behaviour, now);
            let mut h = host(vec![]);
            let (link, _) = run(role, &mut c, &mut h, 10, seed);
            if link.is_done() {
                done += 1;
                let out = link.outcome().unwrap();
                assert_eq!(envelope_titles(&out.received), vec![T3]);
            }
        }
        assert!(done >= 19, "{role:?}: {done} of 20");
    }
}

#[test]
fn silent_console_is_not_an_exchange() {
    let now = Instant::now();
    let mut c = console(Behaviour::Hosts, now);
    c.sends_messages = false;
    let mut h = host(vec![]);
    let (link, events) = run(Role::Lead, &mut c, &mut h, 0, 0);
    assert!(!link.is_done());
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Abandoned {
            reason: "the console stored nothing",
            ..
        }
    )));
}

#[test]
fn link_that_never_comes_up_is_given_up() {
    let now = Instant::now();
    let mut c = console(Behaviour::Hosts, now);
    c.link_delay = Duration::from_secs(3600);
    let mut h = host(vec![]);
    let (link, events) = run(Role::Lead, &mut c, &mut h, 0, 0);
    assert!(!link.is_done());
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Abandoned {
            reason: "the console did not announce its link",
            ..
        }
    )));
}

#[test]
fn declined_console_is_left_alone() {
    let now = Instant::now();
    let mut c = console(Behaviour::Leads, now);
    let mut h = host(vec![]);
    h.plan = None;
    let (link, events) = run(Role::Wait, &mut c, &mut h, 0, 0);
    assert!(!link.is_engaged());
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Declined { .. }))
            .count(),
        1
    );
}
