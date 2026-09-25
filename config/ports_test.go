package config

import (
	"os"
	"regexp"
	"strconv"
	"strings"
	"testing"
)

// portSpan is a framework's ports, control port included.
func portSpan(t *testing.T, framework string) (first, last int) {
	t.Helper()
	ports, err := GetFrameworkBenchmarkPorts(framework)
	if err != nil {
		t.Fatal(err)
	}
	control, err := frameworkControlPort(framework)
	if err != nil {
		t.Fatal(err)
	}
	return ports[0], control
}

func reservedRange(t *testing.T) (first, last int) {
	t.Helper()
	bounds := strings.Split(ReservedPorts, "-")
	if len(bounds) != 2 {
		t.Fatalf("ReservedPorts %q is not first-last", ReservedPorts)
	}
	first, err1 := strconv.Atoi(bounds[0])
	last, err2 := strconv.Atoi(bounds[1])
	if err1 != nil || err2 != nil || first > last {
		t.Fatalf("ReservedPorts %q is not first-last", ReservedPorts)
	}
	return first, last
}

// Every server port, the control ports too, is inside ReservedPorts, so that
// no client socket is ever given one; no two frameworks share a port; and
// ReservedPorts holds nothing past the last framework's control port, since
// every port it holds is one fewer for the client.
func TestPortsReserved(t *testing.T) {
	lo, hi := reservedRange(t)
	owner := map[int]string{}
	first, last := hi, lo
	for _, framework := range FrameworkList {
		a, b := portSpan(t, framework)
		if a < lo || b > hi {
			t.Errorf("%v uses ports %d-%d, outside ReservedPorts %v", framework, a, b, ReservedPorts)
		}
		for p := a; p <= b; p++ {
			if other, ok := owner[p]; ok {
				t.Errorf("port %d is both %v's and %v's", p, other, framework)
			}
			owner[p] = framework
		}
		first, last = min(first, a), max(last, b)
	}
	if first != lo || last != hi {
		t.Errorf("ReservedPorts is %v, the servers use %d-%d", ReservedPorts, first, last)
	}
}

// The copies of the port layout outside this package: benchcli-rust's table,
// quiche's own constant, and the range the scripts reserve.
func TestPortsMatch(t *testing.T) {
	read := func(path string) string {
		b, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		return string(b)
	}
	goRange := func(framework string) string {
		return strings.Replace(Ports[framework], ":", ",", 1)
	}

	client := read("../benchcli-rust/src/config.rs")
	rows := regexp.MustCompile(`\("(\w+)",\s*(\d+),\s*(\d+),\s*(?:true|false)\)`).FindAllStringSubmatch(client, -1)
	if len(rows) != len(FrameworkList) {
		t.Errorf("benchcli-rust/src/config.rs lists %d frameworks, FrameworkList %d", len(rows), len(FrameworkList))
	}
	for _, row := range rows {
		if want, got := goRange(row[1]), row[2]+","+row[3]; want != got {
			t.Errorf("benchcli-rust/src/config.rs: %v on %v, config.Ports on %v", row[1], got, want)
		}
	}

	quiche := regexp.MustCompile(`const PORTS: \(u16, u16\) = \((\d+), (\d+)\);`).FindStringSubmatch(read("../frameworks/quiche/src/main.rs"))
	if quiche == nil {
		t.Error("frameworks/quiche/src/main.rs: no PORTS")
	} else if want, got := goRange(Quiche), quiche[1]+","+quiche[2]; want != got {
		t.Errorf("frameworks/quiche/src/main.rs: PORTS %v, config.Ports %v", got, want)
	}

	script := regexp.MustCompile(`(?m)^ReservedPorts="?([0-9-]+)"?$`).FindStringSubmatch(read("../script/config.sh"))
	if script == nil {
		t.Error("script/config.sh: no ReservedPorts")
	} else if script[1] != ReservedPorts {
		t.Errorf("script/config.sh: ReservedPorts %v, config.ReservedPorts %v", script[1], ReservedPorts)
	}
}
