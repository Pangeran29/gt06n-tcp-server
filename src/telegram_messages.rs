use chrono::{DateTime, Datelike, FixedOffset, NaiveDateTime, TimeZone, Utc};

use crate::bot::{
    AnalyticsDateRange, AnalyticsSessionReport, EngineSession, RideSummary, StoredHeartbeat,
    StoredLocation,
};

const WIB_OFFSET_SECONDS: i32 = 7 * 60 * 60;

pub const MSG_1_BIND_INVALID_IMEI: &str =
    "IMEI harus 15 digit angka. Coba kirim ulang IMEI yang benar ya.";
pub const MSG_2_BIND_ALREADY_BOUND: &str = "Akun Telegram ini sudah terhubung ke perangkat.";
pub const MSG_3_BIND_DEVICE_NOT_FOUND: &str =
    "IMEI ini belum terdaftar di sistem. Cek lagi IMEI-nya lalu coba ulang.";
pub const MSG_4_BIND_DEVICE_ALREADY_TAKEN: &str =
    "Perangkat itu sudah terhubung ke akun Telegram lain.";
pub const MSG_6_NOT_BOUND_USE_START: &str = "Akun Telegram ini belum terhubung ke perangkat. Mulai dari /start dulu ya.";
pub const MSG_7_ANALYTICS_INVALID_DATE: &str = "Format tanggal belum benar. Kirim seperti ini ya:\n2026-05-16";
pub const MSG_8_ANALYTICS_INVALID_RANGE: &str =
    "Format rentang tanggal belum benar. Kirim seperti ini ya:\n2026-05-16 to 2026-05-16";
pub const MSG_9_START_BIND_PROMPT: &str =
    "Halo. Kirim IMEI perangkat kamu dulu ya supaya akun Telegram ini bisa terhubung.";
pub const MSG_10_HELP: &str = "Pantau motor kamu real-time, dapat info motor nyala/mati, dan lihat riwayat perjalanan.\n\n/start - Buka menu utama bot\n/help - Lihat bantuan ini\n/paysupport - Kontak bantuan pembayaran\n/terms - Lihat ketentuan langganan Heartbeats";
pub const MSG_11_PAY_SUPPORT: &str = "Kalau ada pertanyaan, hubungi @jojojows";
pub const MSG_12_TERMS: &str = "Heartbeats adalah layanan pemantauan kendaraan online. Kami menyediakan GPS tracking dengan fitur lengkap lewat sistem langganan bulanan. Kami yang mengelola platform GPS, server, pemakaian data internet, dan aplikasi Heartbeats.\n\nLangganan bulanan sudah termasuk:\n- Tracking motor real-time\n- Notifikasi instan saat mesin ON/OFF\n- Analitik perjalanan (jarak, kecepatan, durasi berkendara, dan visualisasi rute)\n- Fitur lain menyusul\n\nKebijakan pembayaran langganan:\nLangganan harus diperpanjang maksimal 7 hari setelah masa aktif 30 hari berakhir.\nKalau telat bayar, akan ada denda Rp 1.000 per hari sampai pembayaran dilakukan.\n\nKebijakan perangkat GPS:\nPerangkat GPS adalah unit pinjaman.\nKalau berhenti menggunakan Heartbeats, perangkat wajib dikembalikan.\nUntuk pengembalian, silakan hubungi kami lewat /paysupport.\n\nCatatan keamanan perangkat:\nHeartbeats bisa melacak posisi perangkat GPS secara real-time.\nJangan mencoba mencuri, merusak, atau menahan perangkat tanpa izin.";
pub const MSG_14_START_STATUS: &str =
    "Selamat datang di @tryheartbeatsbot\n\nKetik /help kalau mau lihat info lengkapnya.";
pub const MSG_15_SUBSCRIPTION_MENU: &str =
    "Aktifkan langganan Heartbeats biar kamu bisa pantau motor real-time kapan saja.";
pub const MSG_19_INACTIVE_SUB_ENGINE_ON: &str =
    "Motor Dinyalakan\n\nPerpanjang langganan dulu ya supaya bisa akses live tracking, status motor, riwayat perjalanan, dan alert pencurian.";
pub const MSG_20_INACTIVE_SUB_ENGINE_OFF: &str =
    "Motor Dimatikan\n\nPerpanjang langganan dulu ya supaya bisa akses live tracking, status motor, riwayat perjalanan, dan alert pencurian.";
pub const MSG_21_INACTIVE_SUB_FALLBACK: &str =
    "Ada aktivitas dari motor.\n\nPerpanjang langganan dulu ya supaya bisa akses live tracking, status motor, riwayat perjalanan, dan alert pencurian.";
pub const MSG_22_ENGINE_ON_CONFIRMATION: &str =
    "🚨 Engine ON Terdeteksi\n\nMotor Anda baru saja dinyalakan.\nApakah ini Anda?";
pub const MSG_23_RIDE_SAFE: &str =
    "Hati-hati di jalan ya, kami tetap pantau motor kamu di background.";
pub const MSG_24_SESSION_FINISHED: &str = "Sesi perjalanan selesai.";
pub const MSG_25_THEFT_WARNING: &str = "🚨 INDIKASI PENCURIAN\n\nMotor ini dinyalakan bukan oleh Anda. ⚠️ Gerak cepat ya, beberapa menit pertama itu penting banget kalau kejadian pencurian.\n\nTap tombol di bawah untuk mulai live tracking.";
pub const MSG_26_THEFT_LOCATION_MISSING: &str = "Lokasi Terakhir\nLokasi terakhir belum tersedia.";
pub const MSG_28_STREAM_LOCATION_LINK_MISSING: &str = "Link live tracking belum tersedia.";
pub const MSG_29_CONTACT_SUPPORT: &str = "1. Hubungi Call Center 110\n'Halo Polisi, saya ingin melaporkan pencurian motor yang baru saja terjadi. Posisi pelaku sedang terpantau di GPS saya. Mohon bantuan untuk pengejaran.'\n\n2. Datangi SPKT Polsek/Polres\nLangsung ke bagian SPKT (Sentra Pelayanan Kepolisian Terpadu). Tunjukkan aplikasi GPS yang sedang live kepada petugas. Polisi akan langsung berkoordinasi dengan tim Buser/Resmob untuk bergerak ke titik tersebut.\n\n3. Bawa Bukti Kepemilikan\nSiapkan STNK/BPKB (asli atau foto) dan KTP. Polisi butuh ini untuk memastikan itu benar motor Anda sebelum mereka melakukan penindakan atau penangkapan.\n\n4. Minta Pendampingan Unit Lapangan\nSetelah melapor, minta izin untuk mendampingi petugas (di mobil patroli) atau memberikan akses akun GPS Anda kepada petugas agar mereka bisa mengejar target secara akurat.\n\nPENTING: Jangan mendatangi lokasi GPS sendirian. Biarkan polisi yang melakukan tindakan penggerebekan demi keselamatan Anda.";
pub const MSG_31_RIDE_SUMMARY_HISTORY_LINK_MISSING: &str = "Link riwayat rute belum tersedia.";
pub const MSG_32_RIDE_SUMMARY_MAP_LINK_MISSING: &str = "Link lokasi terakhir belum tersedia.";
pub const MSG_33_STATUS_LOCATION_MISSING: &str = "Lokasi belum tersedia.";
pub const MSG_34_STATUS_UNKNOWN: &str = "TIDAK DIKETAHUI";
pub const MSG_35_STATUS_SIGNAL_UNKNOWN: &str = "Tidak diketahui";
pub const MSG_36_ANALYTICS_PICK_RANGE_PREFIX: &str = "Pilih rentang untuk";
pub const MSG_37_ANALYTICS_CUSTOM_DATE_PROMPT: &str =
    "Kirim tanggal custom dalam WIB:\nYYYY-MM-DD\n\nContoh:\n2026-05-16";
pub const MSG_38_ANALYTICS_CUSTOM_RANGE_PROMPT: &str =
    "Kirim rentang tanggal custom dalam WIB:\nYYYY-MM-DD to YYYY-MM-DD\n\nContoh:\n2026-05-16 to 2026-05-16";
pub const MSG_39_ANALYTICS_SESSIONS_MONTH_UNSUPPORTED: &str =
    "History Perjalanan saat ini cuma bisa dibuka per 1 tanggal.";
pub const MSG_43_PLAN_LABEL_BASIC: &str = "Heartbeats Basic";
pub const MSG_44_PLAN_LABEL_OJOL: &str = "Heartbeats Ojol";
pub const MSG_46_PAYMENT_SUCCESS: &str = "Pembayaran Berhasil\n\nAkses Heartbeats kamu sudah aktif sampai {active_until}.\n\nSekarang kamu sudah bisa mulai pantau motor kamu.\nKetik /start untuk mulai atau /help kalau mau lihat fiturnya.";
pub const MSG_47_SUBSCRIPTION_PRE_EXPIRY_REMINDER: &str =
    "Langganan Heartbeats kamu akan segera habis.\n\nPerpanjang dulu sebelum masa aktifnya berakhir ya.\nKalau telat perpanjang, ada denda Rp 1.000 per hari.";
pub const TOAST_1_OPEN_BOT_CHAT: &str = "Buka chat bot-nya dulu lalu coba lagi ya.";
pub const TOAST_2_SUBSCRIPTION_REQUIRED: &str = "Langganan aktif dibutuhkan.";
pub const TOAST_3_BIND_FIRST: &str = "Hubungkan perangkat dulu lewat /start ya.";
pub const TOAST_4_SESSION_NOT_FOUND: &str = "Sesi tidak ditemukan atau sudah tidak aktif.";
pub const TOAST_5_SESSION_MISMATCH: &str =
    "Sesi ini tidak cocok dengan pesan yang dipilih.";
pub const TOAST_6_SESSION_ALREADY_ENDED: &str = "Sesi ini sudah selesai.";
pub const BTN_1_ENGINE_CONFIRM_YES: &str = "Ya, ini saya";
pub const BTN_2_ENGINE_CONFIRM_NO: &str = "Bukan saya";
pub const BTN_3_THEFT_STREAM_LOCATION: &str = "live tracking";
pub const BTN_4_THEFT_HEALTH_CHECK: &str = "cek status";
pub const BTN_5_THEFT_CONTACT_SUPPORT: &str = "hubungi bantuan";
pub const BTN_6_MENU_LIVE_TRACKING: &str = "Live Tracking";
pub const BTN_7_MENU_STATUS_TERKINI: &str = "Status terkini";
pub const BTN_8_MENU_HISTORY_PERJALANAN: &str = "History Perjalanan";
pub const BTN_9_MENU_AKTIVITAS_KENDARAAN: &str = "Aktivitas Kendaraan";
pub const BTN_10_RANGE_SELECT: &str = "Pilih rentang";
pub const BTN_11_RANGE_TODAY: &str = "Hari ini";
pub const BTN_12_RANGE_YESTERDAY: &str = "Kemarin";
pub const BTN_13_RANGE_THIS_MONTH: &str = "Bulan ini";
pub const BTN_14_RANGE_CUSTOM: &str = "Pilih sendiri";
pub const BTN_15_SUBSCRIBE: &str = "Berlangganan";
pub const STICKER_1_ENGINE_ON_FILE_NAME: &str = "AnimatedSticker.tgs";
pub const STICKER_2_BIND_SUCCESS_FILE_NAME: &str = "AnimatedSticker - hi.tgs";
pub const STICKER_3_NOT_SUBSCRIBED_FILE_NAME: &str = "AnimatedSticker - no.tgs";
pub const STICKER_4_THEFT_WARNING_FILE_NAME: &str = "AnimatedSticker - not my motor.tgs";
pub const STICKER_5_PAYMENT_SUCCESS_FILE_NAME: &str = "AnimatedSticker - payment success.tgs";

pub fn msg_5_bind_success(imei: &str) -> String {
    format!("Berhasil. Akun Telegram ini sekarang sudah terhubung ke IMEI {imei}.")
}

pub fn msg_13_unknown_command(command: &str) -> String {
    format!("Command {command} belum dikenali. Ketik /help untuk lihat daftar perintah.")
}

pub fn msg_16_engine_status_notification(heartbeat: &StoredHeartbeat, status: &str) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS)
        .expect("valid WIB offset")
        .from_utc_datetime(&heartbeat.server_received_at.naive_utc());

    match status {
        "on" => format!(
            "Motor Dinyalakan\nKalau ini bukan kamu, segera cek lokasi motor.\n{}",
            wib.format("%d %b %Y - %H:%M WIB")
        ),
        "off" => format!(
            "Motor Dimatikan\nAktivitas terdeteksi pada motor kamu.\n{}",
            wib.format("%d %b %Y - %H:%M WIB")
        ),
        _ => msg_17_heartbeat_notification(heartbeat),
    }
}

pub fn msg_17_heartbeat_notification(heartbeat: &StoredHeartbeat) -> String {
    format!(
        "Update heartbeat\nIMEI: {}\nWaktu server: {}\nStatus mesin: {} (perkiraan)\nTerminal info: {} ({})\nLevel tegangan: {}\nSinyal GSM: {}\nGPS tracking: {}\nACC high: {}\nGetaran terdeteksi: {}",
        heartbeat.imei,
        heartbeat.server_received_at.format("%Y-%m-%d %H:%M:%S UTC"),
        heartbeat.engine_status_guess,
        heartbeat.terminal_info_raw,
        heartbeat.terminal_info_bits,
        heartbeat.voltage_level,
        heartbeat.gsm_signal_strength,
        heartbeat.gps_tracking_on,
        option_bool(heartbeat.acc_high),
        heartbeat.vibration_detected
    )
}

pub fn msg_18_inactive_subscription_engine_status_message(status: &str) -> String {
    match status {
        "on" => MSG_19_INACTIVE_SUB_ENGINE_ON.to_string(),
        "off" => MSG_20_INACTIVE_SUB_ENGINE_OFF.to_string(),
        _ => MSG_21_INACTIVE_SUB_FALLBACK.to_string(),
    }
}

pub fn msg_22_engine_on_confirmation() -> String {
    MSG_22_ENGINE_ON_CONFIRMATION.to_string()
}

pub fn msg_27_theft_engine_off_message(
    latest_location: Option<&StoredLocation>,
    engine_off_at: DateTime<Utc>,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let engine_off_wib = wib.from_utc_datetime(&engine_off_at.naive_utc());
    let location_link =
        latest_location_link(latest_location).unwrap_or_else(|| MSG_33_STATUS_LOCATION_MISSING.to_string());

    format!(
        "🚨 ALERT PENCURIAN\n\nMesin motor kamu baru saja mati di situasi yang terindikasi pencurian.\n\n📍 Lokasi Terakhir Diketahui:\n{}\n\nGPS masih terus aktif dalam mode baterai selama daya perangkat masih ada.\n\nMesin mati terdeteksi pada {}.\n\n⚠️ Segera ambil tindakan: cek live location, bagikan akses tracking, atau hubungi pihak berwajib kalau diperlukan.",
        location_link,
        engine_off_wib.format("%d %b %Y %H:%M WIB"),
    )
}

pub fn msg_28_stream_location_message(live_tracking_link: Option<&str>) -> String {
    let link = live_tracking_link.unwrap_or(MSG_28_STREAM_LOCATION_LINK_MISSING);
    format!(
        "📍 Live Tracking Siap\n\nPantau motor kamu secara real-time di sini:\n{}\n\nLink ini bisa kamu bagikan ke orang yang kamu percaya buat bantu mantau motor.",
        link
    )
}

pub fn msg_30_ride_summary_message(
    session: &EngineSession,
    off_time: DateTime<Utc>,
    summary: Option<&RideSummary>,
    latest_location: Option<&StoredLocation>,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let started_wib = wib.from_utc_datetime(&session.created_at.naive_utc());
    let off_wib = wib.from_utc_datetime(&off_time.naive_utc());
    let total_distance_km = summary.map(|value| value.total_distance_km).unwrap_or(0.0);
    let riding_seconds = summary.map(|value| value.riding_seconds).unwrap_or(0);
    let average_speed_kph = summary.map(|value| value.average_speed_kph).unwrap_or(0.0);
    let history_link = build_history_tracking_link(&session.imei, session.created_at, off_time)
        .unwrap_or_else(|| MSG_31_RIDE_SUMMARY_HISTORY_LINK_MISSING.to_string());
    let latest_map_link = latest_location_link(latest_location)
        .unwrap_or_else(|| MSG_32_RIDE_SUMMARY_MAP_LINK_MISSING.to_string());

    format!(
        "Ringkasan Perjalanan — {}\n\n🏍️ Jarak tempuh {:.2} km\n⏱️ Waktu berkendara {}\n⚡ Kecepatan rata-rata {:.2} km/jam\n\n{} → {} WIB\n\n🗺️ Lihat Rute\n{}\n\n📍 Lokasi Terakhir\n{}",
        started_wib.format("%d %b %Y"),
        total_distance_km,
        format_duration_compact_from_seconds(riding_seconds),
        average_speed_kph,
        started_wib.format("%H:%M"),
        off_wib.format("%H:%M"),
        history_link,
        latest_map_link,
    )
}

pub fn msg_33_latest_motor_status_message(
    session: &EngineSession,
    heartbeat: Option<&StoredHeartbeat>,
    location: Option<&StoredLocation>,
    reference_time: DateTime<Utc>,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let map_link =
        latest_location_link(location).unwrap_or_else(|| MSG_33_STATUS_LOCATION_MISSING.to_string());
    let engine_status = heartbeat
        .map(|value| match value.engine_status_guess.as_str() {
            "on" => "ON",
            "off" => "OFF",
            _ => MSG_34_STATUS_UNKNOWN,
        })
        .unwrap_or(MSG_34_STATUS_UNKNOWN);
    let movement_status = match location.and_then(|value| value.speed_kph) {
        Some(speed) if speed > 0 => format!("BERJALAN {speed} km/jam"),
        Some(_) => "DIAM".to_string(),
        None => MSG_34_STATUS_UNKNOWN.to_string(),
    };
    let signal_status = heartbeat
        .map(|value| connection_status_label(value.gsm_signal_strength))
        .unwrap_or(MSG_35_STATUS_SIGNAL_UNKNOWN);
    let battery_level = heartbeat
        .map(|value| gps_battery_label(value.voltage_level).to_string())
        .unwrap_or_else(|| MSG_35_STATUS_SIGNAL_UNKNOWN.to_string());
    let last_update = heartbeat
        .map(|value| value.server_received_at)
        .into_iter()
        .chain(location.and_then(|value| value.last_seen_at))
        .max()
        .map(|value| format_relative_time_compact(reference_time, value))
        .unwrap_or_else(|| "tidak diketahui".to_string());
    let battery_warning = heartbeat
        .filter(|value| value.voltage_level == 0)
        .map(|_| {
            "\n\n⚠️ Baterai GPS habis. Update baru kemungkinan akan masuk lagi setelah motor dinyalakan."
        })
        .unwrap_or("");
    let session_started_wib = wib.from_utc_datetime(&session.created_at.naive_utc());
    let session_timing = if let Some(resolved_at) = session.resolved_at {
        let resolved_wib = wib.from_utc_datetime(&resolved_at.naive_utc());
        format!(
            "Sesi terakhir selesai pada {} WIB.",
            resolved_wib.format("%H:%M:%S")
        )
    } else {
        format!(
            "Sesi aktif sejak {}",
            session_started_wib.format("%H:%M:%S WIB"),
        )
    };

    format!(
        "📍 Status Motor\n\n{}\n\n{} • Diperbarui {}\nMesin: {} • GPS: {} • Daya: {}\n\n{}{}",
        map_link,
        movement_status,
        last_update,
        engine_status,
        signal_status.to_uppercase(),
        battery_level.to_uppercase(),
        session_timing,
        battery_warning,
    )
}

pub fn msg_34_latest_location_message(location: &StoredLocation) -> String {
    let gps_timestamp = location
        .gps_timestamp
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "tidak diketahui".to_string());
    let last_seen_at = location
        .last_seen_at
        .map(|value| value.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "tidak diketahui".to_string());

    format!(
        "Lokasi Terakhir\nIMEI: {}\nWaktu GPS: {}\nTerakhir dilihat server: {}\nLatitude: {}\nLongitude: {}\nKecepatan: {} km/jam\nArah: {} derajat\nSatelit: {}",
        location.imei,
        gps_timestamp,
        last_seen_at,
        option_f64(location.latitude),
        option_f64(location.longitude),
        option_i32(location.speed_kph),
        option_i32(location.course),
        option_i32(location.satellite_count)
    )
}

pub fn msg_36_choose_range_for(label: &str) -> String {
    format!("{MSG_36_ANALYTICS_PICK_RANGE_PREFIX} {label}.")
}

pub fn msg_40_driving_sessions_report(
    range: &AnalyticsDateRange,
    sessions: &[AnalyticsSessionReport],
    full_day_route_link: Option<&str>,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let report_date = range.started_at.with_timezone(&wib).format("%d %b %Y");
    let total_distance_km = sessions
        .iter()
        .map(|report| report.total_distance_km)
        .sum::<f64>();
    let total_seconds = sessions
        .iter()
        .map(|report| report.riding_seconds)
        .sum::<u64>();
    let longest_ride = sessions
        .iter()
        .max_by_key(|report| report.riding_seconds)
        .map(|report| {
            let start = report.clipped_start.with_timezone(&wib).format("%H:%M");
            let end = report
                .session
                .resolved_at
                .map(|_| {
                report
                        .clipped_end
                        .with_timezone(&wib)
                        .format("%H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "MASIH BERJALAN".to_string());
            format!("{start} → {end}")
        })
        .unwrap_or_else(|| "-".to_string());
    let full_day_route_link = full_day_route_link.unwrap_or("Rute satu hari penuh belum tersedia.");

    let mut lines = vec![
        format!("🛣️ Laporan Perjalanan — {report_date}"),
        String::new(),
        format!(
            "{} sesi • {:.2} km ditempuh • {} waktu berkendara",
            sessions.len(),
            total_distance_km,
            format_duration_minutes_from_seconds(total_seconds)
        ),
        format!("Perjalanan terpanjang: {longest_ride}"),
        String::new(),
    ];

    if sessions.is_empty() {
        lines.push("Tidak ada sesi perjalanan di tanggal ini.".to_string());
        lines.push(String::new());
        lines.push("📍 Rute Seharian".to_string());
        lines.push(full_day_route_link.to_string());
        return lines.join("\n");
    }

    for (index, report) in sessions.iter().enumerate() {
        let start = report.clipped_start.with_timezone(&wib);
        let end = report
            .session
            .resolved_at
            .map(|_| {
                report
                    .clipped_end
                    .with_timezone(&wib)
                    .format("%H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "MASIH BERJALAN".to_string());

        lines.push(format!(
            "{}. {} → {} • {} • {:.2} km",
            index + 1,
            start.format("%H:%M"),
            end,
            format_duration_minutes_from_seconds(report.riding_seconds),
            report.total_distance_km,
        ));
    }

    lines.push(String::new());
    lines.push("📍 Rute Seharian".to_string());
    lines.push(full_day_route_link.to_string());

    lines.join("\n")
}

pub fn msg_41_total_km_report(range: &AnalyticsDateRange, summary: Option<&RideSummary>) -> String {
    let total_distance_km = summary.map(|value| value.total_distance_km).unwrap_or(0.0);
    let average_speed_kph = summary.map(|value| value.average_speed_kph).unwrap_or(0.0);

    format!(
        "Total KM\n{}\n\nTotal jarak: {:.2} km\nKecepatan rata-rata: {:.2} km/jam",
        format_analytics_range_label(range),
        total_distance_km,
        average_speed_kph
    )
}

pub fn msg_42_metrics_report(range: &AnalyticsDateRange, summary: Option<&RideSummary>) -> String {
    let total_distance_km = summary.map(|value| value.total_distance_km).unwrap_or(0.0);
    let total_seconds = summary.map(|value| value.riding_seconds).unwrap_or(0);
    let average_speed_kph = summary.map(|value| value.average_speed_kph).unwrap_or(0.0);

    format!(
        "🏍️ Statistik Perjalanan — {}\n\n{} • {:.2} km ditempuh • {} waktu berkendara • {:.1} km/jam kecepatan rata-rata\n\n⚠️ Jangan lupa rutin cek kondisi motor demi keamanan, termasuk oli mesin, tekanan ban, dan rem.",
        range.label,
        format_ride_stats_date_range(range),
        total_distance_km,
        format_duration_minutes_from_seconds(total_seconds),
        average_speed_kph,
    )
}

pub fn msg_45_total_driving_time_report(
    range: &AnalyticsDateRange,
    total_seconds: u64,
) -> String {
    format!(
        "Total Waktu Berkendara\n{}\n\nTotal waktu berkendara: {}",
        format_analytics_range_label(range),
        format_duration_compact_from_seconds(total_seconds)
    )
}

pub fn msg_46_payment_success(current_period_end_at: Option<DateTime<Utc>>) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let active_until = current_period_end_at
        .map(|value| {
            wib.from_utc_datetime(&value.naive_utc())
                .format("%d %b %Y %H:%M WIB")
                .to_string()
        })
        .unwrap_or_else(|| "tidak diketahui".to_string());
    MSG_46_PAYMENT_SUCCESS.replace("{active_until}", &active_until)
}

pub fn msg_48_subscription_overdue_reminder(fine_amount_idr: i64) -> String {
    format!(
        "Langganan Heartbeats kamu sudah habis.\n\nDenda saat ini: {}\nYuk perpanjang lagi supaya akses penuh kembali aktif.",
        format_idr(fine_amount_idr)
    )
}

pub fn msg_49_payment_link_with_quote(
    plan_label: &str,
    payment_url: &str,
    expires_at: DateTime<Utc>,
    effective_base_amount_idr: i64,
    shipment_fee_idr: i64,
    fine_amount_idr: i64,
    total_amount_idr: i64,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let expires_at = wib.from_utc_datetime(&expires_at.naive_utc());
    let escaped_payment_url = payment_url
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let shipment_line = if shipment_fee_idr > 0 {
        format!("\nBiaya pengiriman: {}", format_idr(shipment_fee_idr))
    } else {
        String::new()
    };
    let fine_line = if fine_amount_idr > 0 {
        format!("\nDenda telat: {}", format_idr(fine_amount_idr))
    } else {
        String::new()
    };
    let total_line = if shipment_fee_idr > 0 || fine_amount_idr > 0 {
        format!("\nTotal: {}", format_idr(total_amount_idr))
    } else {
        String::new()
    };

    format!(
        "{}\n{} - 30 Hari{}{}{}\n\nUntuk aktifkan langganan, selesaikan pembayaran lewat link berikut:\n<tg-spoiler>{escaped_payment_url}</tg-spoiler>\n\nLink pembayaran berlaku sampai: {}",
        plan_label,
        format_idr(effective_base_amount_idr),
        shipment_line,
        fine_line,
        total_line,
        expires_at.format("%d %b %Y %H:%M WIB")
    )
}

pub fn format_idr(amount: i64) -> String {
    let digits = amount.abs().to_string();
    let mut formatted = String::new();
    for (index, character) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push('.');
        }
        formatted.push(character);
    }
    let formatted = formatted.chars().rev().collect::<String>();
    if amount < 0 {
        format!("-Rp {formatted}")
    } else {
        format!("Rp {formatted}")
    }
}

fn format_analytics_range_label(range: &AnalyticsDateRange) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let started_at = range.started_at.with_timezone(&wib);
    let ended_at = range.ended_at.with_timezone(&wib);

    format!(
        "{}\n{} - {} WIB",
        range.label,
        started_at.format("%d %b %Y %H:%M"),
        ended_at.format("%d %b %Y %H:%M")
    )
}

fn format_duration_compact_from_seconds(total_seconds: u64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_duration_minutes_from_seconds(total_seconds: u64) -> String {
    let total_minutes = total_seconds / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn format_ride_stats_date_range(range: &AnalyticsDateRange) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let started_at = range.started_at.with_timezone(&wib);
    let ended_at = range
        .ended_at
        .checked_sub_signed(chrono::Duration::seconds(1))
        .unwrap_or(range.ended_at)
        .with_timezone(&wib);

    if started_at.date_naive() == ended_at.date_naive() {
        return started_at.format("%d %b %Y").to_string();
    }

    if started_at.year() == ended_at.year() {
        format!(
            "{} → {}",
            started_at.format("%d %b"),
            ended_at.format("%d %b %Y")
        )
    } else {
        format!(
            "{} → {}",
            started_at.format("%d %b %Y"),
            ended_at.format("%d %b %Y")
        )
    }
}

fn format_relative_time_compact(reference_time: DateTime<Utc>, event_time: DateTime<Utc>) -> String {
    let duration = reference_time
        .signed_duration_since(event_time)
        .to_std()
        .unwrap_or_default();
    let seconds = duration.as_secs();

    match seconds {
        0..=59 => format!("{seconds}dtk lalu"),
        60..=3599 => format!("{}m lalu", seconds / 60),
        _ => format!("{}j lalu", seconds / 3600),
    }
}

fn connection_status_label(gsm_signal_strength: i32) -> &'static str {
    match gsm_signal_strength.clamp(1, 4) {
        1 => "Lemah",
        2 => "Cukup",
        3 => "Baik",
        4 => "Sangat Baik",
        _ => MSG_35_STATUS_SIGNAL_UNKNOWN,
    }
}

fn gps_battery_label(voltage_level: i32) -> &'static str {
    match voltage_level {
        0 => "Habis",
        1 => "Sangat Rendah",
        2 => "Rendah",
        3 => "Sedang",
        4 => "Penuh",
        _ => MSG_35_STATUS_SIGNAL_UNKNOWN,
    }
}

fn build_history_tracking_link(
    imei: &str,
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
) -> Option<String> {
    let mut url =
        reqwest::Url::parse(&format!("https://hearthbeats-client.vercel.app/live-tracking/{imei}"))
            .ok()?;
    let start_at = start_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let end_at = end_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    url.query_pairs_mut().append_pair("start_at", &start_at);
    url.query_pairs_mut().append_pair("end_at", &end_at);
    Some(url.into())
}

fn latest_location_link(location: Option<&StoredLocation>) -> Option<String> {
    let location = location?;
    let latitude = location.latitude?;
    let longitude = location.longitude?;
    Some(format!(
        "https://maps.google.com/?q={latitude:.6},{longitude:.6}"
    ))
}

fn option_f64(value: Option<f64>) -> String {
    value.map(|value| value.to_string())
        .unwrap_or_else(|| "tidak diketahui".to_string())
}

fn option_i32(value: Option<i32>) -> String {
    value.map(|value| value.to_string())
        .unwrap_or_else(|| "tidak diketahui".to_string())
}

fn option_bool(value: Option<bool>) -> String {
    value.map(|value| value.to_string())
        .unwrap_or_else(|| "tidak diketahui".to_string())
}

#[allow(dead_code)]
fn _unused(_value: NaiveDateTime) {}
