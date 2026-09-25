package config

import "testing"

// BENCH_SERVER_HOST may be a hostname, an IPv4 address or an IPv6 one; only
// the last needs brackets before a port can follow it in a URL.
func TestURLHostBracketsIPv6(t *testing.T) {
	for in, want := range map[string]string{
		"127.0.0.1":      "127.0.0.1",
		"bench-server-1": "bench-server-1",
		"10.0.0.2":       "10.0.0.2",
		"::1":            "[::1]",
		"fe80::1":        "[fe80::1]",
		"[fe80::1]":      "[fe80::1]",
	} {
		if got := urlHost(in); got != want {
			t.Errorf("urlHost(%q) = %q, want %q", in, got, want)
		}
	}
}

// The clients dial host:port, which for an IPv6 host needs the brackets
// whether or not BENCH_SERVER_HOST came with them; the control routes are the
// port after the last benchmark one.
func TestFrameworkAddrs(t *testing.T) {
	for _, ip := range []string{"::1", "[::1]"} {
		addrs, err := GetFrameworkBenchmarkAddrs(QuicGo, ip)
		if err != nil {
			t.Fatal(err)
		}
		if len(addrs) != 50 || addrs[0] != "[::1]:3201" || addrs[49] != "[::1]:3250" {
			t.Errorf("GetFrameworkBenchmarkAddrs(%q) = %v ... %v", ip, addrs[0], addrs[len(addrs)-1])
		}
	}
	if got, _ := FrameworkControlAddr(QuicGo, "::1"); got != "http://[::1]:3251" {
		t.Errorf("FrameworkControlAddr = %q", got)
	}
	if got, _ := GetFrameworkControlServerAddr(Fib); got != ":3051" {
		t.Errorf("GetFrameworkControlServerAddr = %q", got)
	}
	if _, err := GetFrameworkBenchmarkPorts("gorilla"); err == nil {
		t.Error("GetFrameworkBenchmarkPorts accepted a framework it does not know")
	}
}
