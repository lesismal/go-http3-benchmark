//go:build linux && amd64

package main

import (
	"context"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"

	"go-http3-benchmark/config"
	"go-http3-benchmark/frameworks"
	"go-http3-benchmark/logging"

	"github.com/guno1928/alos-http/core"
)

func main() {
	frameworks.Init(config.Alos)
	control := frameworks.StartControlServer()
	defer control.Close()

	certFile, keyFile, cleanup := writeCertificate()
	defer cleanup()

	// core.Config takes one address, so one server per port, each serving
	// HTTP/3 alone: ListenAndServeQUIC opens its UDP sockets and nothing
	// else, no TCP listener and no HTTP-to-HTTPS redirect. The CPUs are
	// shared out over the ports as SO_REUSEPORT sockets, each with a receive
	// loop of its own: one per port on fifty ports, and on one port (see
	// BENCH_PORTS) one per CPU, alos's own default for a single address. The
	// request streams all go to alos's one process-wide pool of stream
	// workers, whichever server they arrive on.
	addrs := frameworks.ServerAddrs()
	procs := runtime.GOMAXPROCS(0)
	listeners := max(1, (procs+len(addrs)-1)/len(addrs))
	logging.Printf("%v server: %d ports, %d UDP sockets and receive loops each", config.Alos, len(addrs), listeners)
	servers := make([]*core.Server, 0, len(addrs))
	for _, addr := range addrs {
		s := core.New(core.Config{
			Addr:        addr,
			Listeners:   listeners,
			WorkerCount: procs,
			// The certificate every server here signs its handshakes with
			// (frameworks.TLSConfig), rather than the self-signed one alos
			// would otherwise write into the working directory.
			TLSCertFile: certFile,
			TLSKeyFile:  keyFile,
			// QUIC's max_idle_timeout, as every server is given it.
			IdleTimeout: *frameworks.IdleTimeout,
			// The datagram size quiche and fib are given, where alos would
			// otherwise keep to 1200 bytes.
			QUICMaxUDPPayload: 1350,
			// Off the limits a public server wants and a benchmark client
			// dialing every connection from one address trips: alos
			// defaults to 8192 connections in all and 20 per client address,
			// and resets streams once 64 MiB of request bodies are buffered
			// across the server.
			MaxConns:             -1,
			MaxConnsPerIP:        -1,
			MaxInFlightBodyBytes: -1,
			ReadTimeout:          -1,
			ReadHeaderTimeout:    -1,
			WriteTimeout:         -1,
			MaxBodySize:          max(4<<20, int64(*frameworks.Payload)*2),
		})
		// Any method, as the other servers answer it: the client checks each
		// connection it dials with a GET before the benchmark POSTs.
		s.Router.ANY(config.EchoPath, onRequest)
		servers = append(servers, s)
	}

	var wg sync.WaitGroup
	for i, s := range servers {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if err := s.ListenAndServeQUIC(); err != nil {
				logging.Fatalf("listen %v failed: %v", addrs[i], err)
			}
		}()
	}
	// ListenAndServeQUIC binds its sockets in its own goroutine and says
	// nothing when it has, and the scripts take the line LogListening writes
	// to mean every port is bound; so wait for the kernel to list them.
	waitBound(addrs, listeners)
	frameworks.LogListening(addrs)

	frameworks.WaitSignal()
	// Shutdown drains each server's connections, and a QUIC receive loop
	// may not return once it has; so bound the whole of it, and exit
	// regardless once the time is up rather than outlive the run.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	done := make(chan struct{})
	go func() {
		for _, s := range servers {
			_ = s.Shutdown(ctx)
		}
		wg.Wait()
		close(done)
	}()
	select {
	case <-done:
	case <-ctx.Done():
		logging.Printf("%v server: shutdown did not finish in time, exiting anyway", config.Alos)
	}
}

func onRequest(req *core.Request, resp *core.Response) {
	// The body has been read off the stream whole; Bytes copies it into the
	// response, which does not hold on to the request's buffer.
	resp.Status(200).Bytes(req.Body)
}

// waitBound returns once every address in addrs has listeners UDP sockets
// bound to its port in /proc/net/udp (alos's QUIC sockets are IPv4), giving
// up with a fatal error after 30 seconds.
func waitBound(addrs []string, listeners int) {
	want := make(map[string]int, len(addrs))
	for _, addr := range addrs {
		_, port, _ := strings.Cut(addr, ":")
		n, _ := strconv.Atoi(port)
		want[fmt.Sprintf("%04X", n)] = listeners
	}
	deadline := time.Now().Add(30 * time.Second)
	for {
		data, err := os.ReadFile("/proc/net/udp")
		if err != nil {
			logging.Fatalf("reading /proc/net/udp failed: %v", err)
		}
		got := map[string]int{}
		for _, line := range strings.Split(string(data), "\n")[1:] {
			fields := strings.Fields(line)
			if len(fields) < 2 {
				continue
			}
			if _, port, ok := strings.Cut(fields[1], ":"); ok {
				got[port]++
			}
		}
		bound := true
		for port, n := range want {
			if got[port] < n {
				bound = false
				break
			}
		}
		if bound {
			return
		}
		if time.Now().After(deadline) {
			logging.Fatalf("%v server: UDP sockets not all bound after 30s", config.Alos)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// writeCertificate writes frameworks.TLSConfig's certificate and key to PEM
// files, which is how alos takes a certificate before it starts.
func writeCertificate() (certFile, keyFile string, cleanup func()) {
	cert := frameworks.TLSConfig().Certificates[0]
	dir, err := os.MkdirTemp("", "alos-h3-cert-")
	if err != nil {
		logging.Fatalf("creating the certificate directory failed: %v", err)
	}
	cleanup = func() { os.RemoveAll(dir) }
	var certPEM []byte
	for _, der := range cert.Certificate {
		certPEM = append(certPEM, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})...)
	}
	keyDER, err := x509.MarshalPKCS8PrivateKey(cert.PrivateKey)
	if err != nil {
		cleanup()
		logging.Fatalf("encoding the private key failed: %v", err)
	}
	keyPEM := pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER})
	certFile = filepath.Join(dir, "cert.pem")
	keyFile = filepath.Join(dir, "key.pem")
	if err := os.WriteFile(certFile, certPEM, 0o600); err != nil {
		cleanup()
		logging.Fatalf("writing the certificate failed: %v", err)
	}
	if err := os.WriteFile(keyFile, keyPEM, 0o600); err != nil {
		cleanup()
		logging.Fatalf("writing the private key failed: %v", err)
	}
	return certFile, keyFile, cleanup
}
