use std::thread;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rotor::{Loop, Config, Machine};
use rotor::mio::tcp::TcpStream;
use rotor_carbon::{Fsm as Carbon, connect_ip};
use rotor_tools::timer::{IntervalFunc, interval_func};
use rotor_tools::loop_ext::{LoopExt, LoopInstanceExt};

use configs::Configs;
use stats::Stats;
use carbon;

pub struct Context {
    stats: Arc<RwLock<Stats>>,
}

rotor_compose!(enum Fsm/Seed<Context> {
    Carbon(Carbon<Context, TcpStream>),
    CarbonTimer(IntervalFunc<Context>),
});


// All new async things should be in rotor main loop
pub fn start(configs: &Configs, stats: Arc<RwLock<Stats>>) {
    let loop_creator = Loop::new(&Config::new()).unwrap();
    let mut loop_inst = loop_creator.instantiate(Context {
        stats: stats,
    });

    for cfg in &configs.carbon {
        let sink = loop_inst.add_and_fetch(Fsm::Carbon, |scope| {
            info!("Connecting to carbon at {}:{} (interval {})",
                cfg.host, cfg.port, cfg.interval);
            connect_ip(
                format!("{}:{}", cfg.host, cfg.port).parse().unwrap(),
                scope)
        }).unwrap();
        let cfg = cfg.clone();
        loop_inst.add_machine_with(|scope| {
            interval_func(scope,
                Duration::new(cfg.interval as u64, 0), move |ctx| {
                        debug!("Sending data to carbon {}:{}",
                            cfg.host, cfg.port);
                        carbon::send(&mut sink.sender(), &cfg,
                                     &*ctx.stats.read()
                                       .expect("Can't lock stats"));
                }).wrap(Fsm::CarbonTimer)
        }).unwrap();
    }
    thread::spawn(move || {
        loop_inst.run().unwrap();
    });
}
