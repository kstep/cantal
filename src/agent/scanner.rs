use std::sync::{Arc, RwLock};
use std::io::Write;

use mio;
use nix::unistd::getpid;
use libc::usleep;
use cbor::Encoder as Mencoder;

use super::server;
use super::stats::Stats;
use super::scan::Tip;
use super::scan::machine;
use super::scan::processes;
use super::scan::values;
use super::scan::time_ms;
use super::util::Cell;
use super::storage::Buffer;
use super::deps::{Dependencies, LockedDeps};
use cantal::Value;


const SNAPSHOT_INTERVAL: u64 = 60000;


pub fn scan_loop(deps: Dependencies)
{
    let stats: &RwLock<Stats> = &*deps.copy();
    let cell = deps.get::<Arc<Cell<Buffer>>>().map(|x| &*x);
    let server_msg = deps.get::<mio::Sender<server::Message>>().unwrap();
    let mut last_store = time_ms();
    let mut last_hourly = last_store / 3_600_000;
    let mut process_cache = processes::ReadCache::new();
    let mut values_cache = values::ReadCache::new();
    stats.write().map(|mut s| { s.pid = getpid(); })
        .ok().expect("Stats must be readable still");
    loop {
        let start = time_ms();
        let mut tip = Tip::new();

        let boot_time = machine::read(&mut tip);

        let processes = processes::read(&mut process_cache);
        processes::write_tip(&mut tip, &processes);
        values::read(&mut tip, &mut values_cache, &processes);

        let scan_duration = (time_ms() - start) as u32;

        if let Ok(ref mut stats) = stats.write() {
            stats.scan_duration = scan_duration;
            debug!("Got {} values and {} processes in {} ms",
                tip.map.len(), processes.len(), scan_duration);

            // TODO(tailhook) use drain-style iterator and push to both
            // at once, so we don't need clone (each metric)
            stats.history.fine.push((start, scan_duration), tip.map.iter()
                .filter(|&(k, v)| matches!(v, &Value::State(_, _))));
            stats.history.tip.push((start, scan_duration), tip.map.iter()
                .filter(|&(k, v)| !matches!(v, &Value::State(_, _))));

            stats.last_scan = start;
            stats.boot_time = boot_time.or(stats.boot_time);
            stats.processes = processes;

            if start - last_store > SNAPSHOT_INTERVAL {
                last_store = start;
                let hourly = start / 3_600_000;
                let mut snapshot = None;
                if hourly > last_hourly {
                    stats.history.truncate_by_time(last_hourly*3_600_000);
                    snapshot = Some(format!("hourly-{}", hourly));
                    last_hourly = hourly;
                }
                if let Some(cell) = cell {
                    cell.put(Buffer {
                        timestamp: start,
                        snapshot: snapshot,
                        data: stats.history.encode(),
                    });
                }
            }
        }
        server_msg.send(server::Message::ScanComplete)
            .map_err(|_| error!("Error sending ScanComplete msg"))
            .ok();

        unsafe { usleep(((2000 - time_ms() as i64 % 2000)*1000) as u32) };
    }
}
