// Package frameworks is what every benchmark server shares: the flags
// script/benchmark.sh forwards to them, the TLS certificate HTTP/3 needs, the
// transport limits every server is given alike, and the control server that
// the client reads the server's pid, CPU and memory from.
package frameworks

import (
	"errors"
	"flag"
	"net/http"
	"os"
	"os/signal"
	"runtime/debug"
	"syscall"
	"time"

	"go-http3-benchmark/config"
	"go-http3-benchmark/logging"
)

// The flags script/benchmark.sh forwards to every server. Each server defines
// all of them, whether or not it has a use for each, since one it did not
// define would stop it with "flag provided but not defined".
var (
	Payload  = flag.Int("b", 1024, `expected request body size, which sizes the servers' read buffers`)
	memLimit = flag.Int64("m", 1024*1024*1024*2, `memory limit`)
	// MaxStreams is how many request streams a client may have open on one
	// connection at once: QUIC's MAX_STREAMS for bidirectional streams. Both
	// servers default to 100, and both are given the same value here, so that
	// neither is measured with more concurrency per connection than the other.
	MaxStreams = flag.Int("streams", 100, `max concurrent request streams per connection`)
	// IdleTimeout closes a connection that has been silent this long. It is
	// long, since a million-connection run leaves the first connections idle
	// for as long as it takes to dial the rest; the client also sends a PING
	// on a connection that has been quiet for a quarter of it.
	IdleTimeout = flag.Duration("idle", 120*time.Second, `QUIC max idle timeout`)
	framework   string
)

// Init parses the flags and applies the ones every server shares. framework
// is the server's name as config knows it.
func Init(name string) {
	flag.Parse()
	framework = name
	debug.SetMemoryLimit(*memLimit)
	logging.Printf("%v server: payload=%v, streams=%v, idle=%v, memory limit=%v",
		name, *Payload, *MaxStreams, *IdleTimeout, *memLimit)
}

// ServerAddrs is the addresses the framework's server listens on for the
// benchmark.
func ServerAddrs() []string {
	addrs, err := config.GetFrameworkServerAddrs(framework)
	if err != nil {
		logging.Fatalf("GetFrameworkServerAddrs(%v) failed: %v", framework, err)
	}
	return addrs
}

// LogListening says which ports a server is serving, once it is.
func LogListening(addrs []string) {
	logging.Printf("%v server: listening on %d UDP ports, %v to %v",
		framework, len(addrs), addrs[0], addrs[len(addrs)-1])
}

// StartControlServer serves the control routes on the framework's control
// port, on a net/http server of its own; see
// config.GetFrameworkControlServerAddr.
func StartControlServer() *http.Server {
	addr, err := config.GetFrameworkControlServerAddr(framework)
	if err != nil {
		logging.Fatalf("GetFrameworkControlServerAddr(%v) failed: %v", framework, err)
	}
	mux := http.NewServeMux()
	HandleCommon(mux)
	server := &http.Server{Addr: addr, Handler: mux}
	go func() {
		if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			logging.Fatalf("control server on %v exit: %v", addr, err)
		}
	}()
	return server
}

// WaitSignal blocks until the server is told to stop: SIGINT, which
// script/killone.sh sends, or SIGTERM.
func WaitSignal() {
	interrupt := make(chan os.Signal, 1)
	signal.Notify(interrupt, os.Interrupt, syscall.SIGTERM)
	<-interrupt
	logging.Printf("%v server: exit", framework)
}
