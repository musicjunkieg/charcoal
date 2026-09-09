use charcoal::observability::cpu_sample::parse_proc_stat_cpu_ticks;

/// A real `/proc/self/stat` line. Field 2 (`comm`) is parenthesised and may
/// contain spaces, so the parser must split after the last `)` — fields 14
/// (utime) and 15 (stime) are counted from field 1 = pid.
const SAMPLE: &str = "12345 (charcoal web) S 1 12345 12345 0 -1 4194560 8123 0 0 0 4200 1300 0 0 20 0 9 0 1234567 987654321 45678 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";

#[test]
fn parses_utime_plus_stime() {
    // utime = 4200, stime = 1300
    assert_eq!(parse_proc_stat_cpu_ticks(SAMPLE), Some(5500));
}

#[test]
fn comm_with_spaces_and_parens_does_not_shift_fields() {
    let weird = SAMPLE.replace("(charcoal web)", "(char (coal) )web)");
    assert_eq!(parse_proc_stat_cpu_ticks(&weird), Some(5500));
}

#[test]
fn short_or_garbage_input_is_none() {
    assert_eq!(parse_proc_stat_cpu_ticks(""), None);
    assert_eq!(parse_proc_stat_cpu_ticks("1 (x) S 1 2"), None);
    assert_eq!(
        parse_proc_stat_cpu_ticks("no parens at all 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15"),
        None
    );
}
