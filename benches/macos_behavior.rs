#![cfg(target_os = "macos")]

use criterion::{Criterion, criterion_group, criterion_main};
use sysproxy::{Autoproxy, Sysproxy};
use system_configuration::core_foundation::string::CFString;

fn get_valid_service() -> CFString {
    CFString::from_static_string("Wi-Fi")
}

fn bench_get_system_proxy(c: &mut Criterion) {
    c.bench_function("get_system_proxy", |b| b.iter(Sysproxy::get_system_proxy));
}

fn bench_get_http(c: &mut Criterion) {
    let service = get_valid_service();

    c.bench_function("get_http", |b| {
        b.iter(|| Sysproxy::get_http(&service, None))
    });
}

fn bench_get_https(c: &mut Criterion) {
    let service = get_valid_service();
    c.bench_function("get_https", |b| {
        b.iter(|| Sysproxy::get_https(&service, None))
    });
}

fn bench_get_socks(c: &mut Criterion) {
    let service = get_valid_service();
    c.bench_function("get_socks", |b| {
        b.iter(|| Sysproxy::get_socks(&service, None))
    });
}

fn bench_get_bypass(c: &mut Criterion) {
    let service = get_valid_service();
    c.bench_function("get_bypass", |b| {
        b.iter(|| Sysproxy::get_bypass(&service, None))
    });
}

fn bench_can_lock_scpreferences(c: &mut Criterion) {
    c.bench_function("can_lock_scpreferences", |b| {
        b.iter(Sysproxy::can_lock_scpreferences)
    });
}

fn bench_set_http(c: &mut Criterion) {
    let service = get_valid_service().to_string();
    let proxy = Sysproxy {
        enable: false,
        host: "127.0.0.1".into(),
        port: 8080,
        bypass: "".into(),
    };
    c.bench_function("set_http", |b| b.iter(|| proxy.set_http(&service)));
}

fn bench_set_https(c: &mut Criterion) {
    let service = get_valid_service().to_string();
    let proxy = Sysproxy {
        enable: false,
        host: "127.0.0.1".into(),
        port: 8080,
        bypass: "".into(),
    };
    c.bench_function("set_https", |b| b.iter(|| proxy.set_https(&service)));
}

fn bench_set_socks(c: &mut Criterion) {
    let service = get_valid_service().to_string();
    let proxy = Sysproxy {
        enable: false,
        host: "127.0.0.1".into(),
        port: 8080,
        bypass: "".into(),
    };
    c.bench_function("set_socks", |b| b.iter(|| proxy.set_socks(&service)));
}

fn bench_set_bypass(c: &mut Criterion) {
    let service = get_valid_service().to_string();
    let proxy = Sysproxy {
        enable: false,
        host: "".into(),
        port: 0,
        bypass: "127.0.0.1,localhost".into(),
    };
    c.bench_function("set_bypass", |b| b.iter(|| proxy.set_bypass(&service)));
}

fn bench_set_system_proxy(c: &mut Criterion) {
    let proxy = Sysproxy {
        enable: false,
        host: "127.0.0.1".into(),
        port: 8080,
        bypass: "".into(),
    };
    c.bench_function("set_system_proxy", |b| b.iter(|| proxy.set_system_proxy()));
}

fn bench_get_auto_proxy(c: &mut Criterion) {
    c.bench_function("get_auto_proxy", |b| b.iter(Autoproxy::get_auto_proxy));
}

fn bench_set_auto_proxy(c: &mut Criterion) {
    let proxy = Autoproxy {
        enable: false,
        url: "".into(),
    };
    c.bench_function("set_auto_proxy", |b| b.iter(|| proxy.set_auto_proxy()));
}

fn custom_config() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(120))
}

criterion_group! {
    name = benches;
    config = custom_config();
    targets =
        bench_get_system_proxy,
        bench_get_http,
        bench_get_https,
        bench_get_socks,
        bench_get_bypass,
        bench_can_lock_scpreferences,
        bench_set_http,
        bench_set_https,
        bench_set_socks,
        bench_set_bypass,
        bench_set_system_proxy,
        bench_get_auto_proxy,
        bench_set_auto_proxy
}
criterion_main!(benches);
