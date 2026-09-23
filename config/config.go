package config

import (
	"fmt"
	"net"
	"strconv"
	"strings"
	"time"
)

// InitArgs is the body of a /init request: how often the server samples its
// own CPU and memory. The client sends it as JSON, with PsInterval in
// nanoseconds, the way encoding/json writes a time.Duration.
type InitArgs struct {
	PsInterval time.Duration
}

// Every framework this benchmark knows, by the name that -f, the server binary
// and the report row all take.
//
// This list, Ports and FrameworkList below are kept in framework-name order,
// as are the framework lists in script/config.sh,
// script/1m_conns_benchmark.sh and benchcli-rust/src/config.rs, so that a
// framework sits in the same place in all of them and a new one has one
// obvious place to go in each.
const (
	Fib    = "fib"
	QuicGo = "quicgo"
	Quiche = "quiche"
)

// Ports is the range of UDP benchmark ports each framework's server listens
// on. Fifty of them, so that a client dialing a million connections from one
// address, each from a UDP socket of its own, does not run out of ephemeral
// ports towards any one of them.
//
// benchcli-rust/src/config.rs and frameworks/quiche/src/main.rs carry the
// same ports, since neither is built from this package.
var Ports = map[string]string{
	Fib:    "11001:11050",
	QuicGo: "12001:12050",
	Quiche: "13001:13050",
}

// FrameworkLangs is the language each framework's server is written in, as
// the Lang column of the report tables shows it: "go", "rust", "c++" and so
// on.
var FrameworkLangs = map[string]string{
	Fib:    "go",
	QuicGo: "go",
	Quiche: "rust",
}

// FrameworkList is every framework, in framework-name order. It is also the
// row order of a -sort=framework report, which is what puts a framework on the
// same row in every table and across runs, whatever it scored.
var FrameworkList = []string{
	Fib,
	QuicGo,
	Quiche,
}

// EchoPath is the route every server answers the benchmark on: the response
// body is the request body, byte for byte, with a Content-Length.
const EchoPath = "/echo"

func GetFrameworkBenchmarkPorts(framework string) ([]int, error) {
	portRange, ok := Ports[framework]
	if !ok {
		return nil, fmt.Errorf("unknown framework %q", framework)
	}
	bounds := strings.Split(portRange, ":")
	minPort, err := strconv.Atoi(bounds[0])
	if err != nil {
		return nil, err
	}
	maxPort, err := strconv.Atoi(bounds[1])
	if err != nil {
		return nil, err
	}
	ports := []int{}
	for i := minPort; i <= maxPort; i++ {
		ports = append(ports, i)
	}
	return ports, nil
}

// GetFrameworkServerAddrs is the addresses a server listens on for the
// benchmark: every interface, one UDP address per port.
func GetFrameworkServerAddrs(framework string) ([]string, error) {
	ports, err := GetFrameworkBenchmarkPorts(framework)
	if err != nil {
		return nil, err
	}
	addrs := make([]string, 0, len(ports))
	for _, port := range ports {
		addrs = append(addrs, fmt.Sprintf(":%d", port))
	}
	return addrs, nil
}

// GetFrameworkControlServerAddr is the address a server's control routes -
// /init, /ps and the pprof ones - listen on: TCP, on the port after its last
// benchmark port. Every framework serves them there on a net/http server of
// its own, so that the routes a client reads its resource columns from are
// the same code for all of them and never queue behind benchmark requests,
// and so that the framework being measured serves nothing but /echo.
func GetFrameworkControlServerAddr(framework string) (string, error) {
	port, err := frameworkControlPort(framework)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf(":%d", port), nil
}

// urlHost brackets a bare IPv6 literal so that it can carry a port in a URL.
// BENCH_SERVER_HOST may be an address or a hostname, and an IPv6 address
// without this comes out as http://fe80::1:12051/ps, which parses as neither
// host nor port.
func urlHost(ip string) string {
	if strings.Contains(ip, ":") && !strings.HasPrefix(ip, "[") {
		return "[" + ip + "]"
	}
	return ip
}

// GetFrameworkBenchmarkAddrs is the host:port a client dials for each of the
// framework's benchmark ports.
func GetFrameworkBenchmarkAddrs(framework, ip string) ([]string, error) {
	ports, err := GetFrameworkBenchmarkPorts(framework)
	if err != nil {
		return nil, err
	}
	host := strings.Trim(ip, "[]")
	addrs := make([]string, 0, len(ports))
	for _, port := range ports {
		addrs = append(addrs, net.JoinHostPort(host, strconv.Itoa(port)))
	}
	return addrs, nil
}

// frameworkControlPort is the port a framework's control routes listen on:
// the one after its last benchmark port.
func frameworkControlPort(framework string) (int, error) {
	ports, err := GetFrameworkBenchmarkPorts(framework)
	if err != nil {
		return 0, err
	}
	return ports[len(ports)-1] + 1, nil
}

// FrameworkControlAddr is the base URL of those routes, as a client reaches
// them.
func FrameworkControlAddr(framework, ip string) (string, error) {
	port, err := frameworkControlPort(framework)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("http://%v:%v", urlHost(ip), port), nil
}
