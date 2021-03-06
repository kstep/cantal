use std::sync::{Arc, RwLock};
use std::io::Write;

use mio;
use libc::usleep;
use probor::{Encoder, Encodable};

use super::server;
use super::stats::Stats;
use super::scan::Tip;
use super::scan::machine;
use super::scan::processes;
use super::scan::connections;
use super::scan::values;
use super::scan::time_ms;
use super::scan::cgroups;
use super::deps::{Dependencies, LockedDeps};
use cantal::Value;
use history::VersionInfo;
use storage::{Storage, MetricBuffer};


const SNAPSHOT_INTERVAL: u64 = 60000;

pub fn scan_loop(deps: Dependencies, interval: u32)
{
    let stats: &RwLock<Stats> = &*deps.copy();
    let storage = deps.get::<Arc<Storage>>().map(|x| &*x);
    let server_msg = deps.get::<mio::Sender<server::Message>>().unwrap();
    let mut last_store = time_ms();
    let mut last_hourly = last_store / 3_600_000;
    let mut process_cache = processes::ReadCache::new();
    let mut values_cache = values::ReadCache::new();
    let mut last_buffer_size = 16 << 10;
    loop {
        let start = time_ms();
        let mut tip = Tip::new();

        let boot_time = machine::read(&mut tip);

        let cgroups = cgroups::read();
        let processes = processes::read(&mut process_cache, &cgroups);
        let connections = connections::read();
        processes::write_tip(&mut tip, &processes, &cgroups);
        values::read(&mut tip, &mut values_cache, &processes, &cgroups);

        let scan_duration = (time_ms() - start) as u32;

        if let Ok(ref mut stats) = stats.write() {
            stats.scan_duration = scan_duration;
            debug!("Got {} values and {} processes in {} ms",
                tip.map.len(), processes.len(), scan_duration);

            // TODO(tailhook) use drain-style iterator and push to both
            // at once, so we don't need clone (each metric)
            stats.history.tip.push((start, scan_duration), tip.map.iter()
                .filter(|&(_, v)| matches!(v, &Value::State(_))));
            stats.history.fine.push((start, scan_duration), tip.map.iter()
                .filter(|&(_, v)| !matches!(v, &Value::State(_))));

            stats.last_scan = start;
            stats.boot_time = boot_time.or(stats.boot_time);
            stats.processes = processes;
            stats.connections = connections;

            if start - last_store > SNAPSHOT_INTERVAL {
                last_store = start;
                let hourly = start / 3_600_000;
                let mut snapshot = None;
                if hourly > last_hourly {
                    stats.history.truncate_by_time(last_hourly*3_600_000);
                    snapshot = Some(format!("hourly-{}", hourly));
                    last_hourly = hourly;
                }

                // Preallocate a buffer of same size as previous one, since
                // it's expected about same size. But add few kb, so that
                // 99% of the time no further allocations are necessary
                let mut enc = Encoder::new(
                    Vec::with_capacity(last_buffer_size + 16384));
                VersionInfo::current().encode(&mut enc)
                .and_then(|()| stats.history.encode(&mut enc))
                .map(|()| {
                    let buf = enc.into_writer();
                    last_buffer_size = buf.len();

                    if let Some(storage) = storage {
                        storage.store_metrics(MetricBuffer {
                            timestamp: start,
                            snapshot: snapshot,
                            data: buf.into_boxed_slice(),
                        });
                    }
                }).map_err(|e| error!("Can't encode history: {}", e)).ok();
            }
        }
        server_msg.send(server::Message::ScanComplete)
            .map_err(|_| error!("Error sending ScanComplete msg"))
            .ok();

        unsafe { usleep(((interval as i64 - time_ms() as i64 % interval as i64)*1000) as u32) };
    }
}
