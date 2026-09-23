package frameworks

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/pprof"
	"os"
	"sync"
	"sync/atomic"
	"time"

	"go-http3-benchmark/config"
	"go-http3-benchmark/logging"

	"github.com/lesismal/perf"
)

var (
	psCounter *perf.PSCounter
	psStarted atomic.Bool
)

// HandleCommon registers the control routes: pprof, /init, which starts the
// server sampling its own CPU and memory and answers with its pid, and /ps,
// which answers with those samples.
func HandleCommon(mux *http.ServeMux) {
	mux.HandleFunc("/debug/pprof/", pprof.Index)
	mux.HandleFunc("/debug/pprof/cmdline", pprof.Cmdline)
	mux.HandleFunc("/debug/pprof/profile", pprof.Profile)
	mux.HandleFunc("/debug/pprof/symbol", pprof.Symbol)
	mux.HandleFunc("/debug/pprof/trace", pprof.Trace)

	var err error
	psCounter, err = perf.NewPSCounter(os.Getpid())
	if err != nil {
		logging.Fatalf("perf.NewPSCounter failed: %v", err)
	}

	mux.HandleFunc("/init", func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		var args config.InitArgs
		json.Unmarshal(body, &args)
		// Once, however many times /init arrives: a client that retried the
		// request because its own read failed can deliver it twice, and a
		// second Start would reset the sample slices under the goroutines
		// already appending to them.
		if psStarted.CompareAndSwap(false, true) {
			go func() {
				psCounter.Start(perf.PSCountOptions{
					CountCPU: true,
					CountMEM: true,
					CountIO:  true,
					CountNET: true,
					Interval: args.PsInterval,
				})
				time.Sleep(args.PsInterval)
			}()
		} else {
			logging.Printf("/init called again; the ps counter is already running")
		}

		fmt.Fprintf(w, "%d", os.Getpid())
	})

	mux.HandleFunc("/ps", func(w http.ResponseWriter, r *http.Request) {
		b, _ := json.Marshal(psCounter)
		w.Write(b)
	})
}

// BodyPool holds the buffers the servers read a request body into before
// echoing it, sized by -b, so that echoing a body costs no allocation of its
// own in either of them.
var BodyPool = sync.Pool{
	New: func() any {
		buf := make([]byte, *Payload)
		return &buf
	},
}

// ReadBody reads a request body of contentLength bytes - or of unknown length,
// for -1 - into a buffer from BodyPool. The caller returns bufp to the pool
// once the body has been written back.
func ReadBody(body io.Reader, contentLength int64) (data []byte, bufp *[]byte, err error) {
	bufp = BodyPool.Get().(*[]byte)
	buf := *bufp
	if contentLength >= 0 {
		if int64(cap(buf)) < contentLength {
			buf = make([]byte, contentLength)
			*bufp = buf
		}
		buf = buf[:contentLength]
		_, err = io.ReadFull(body, buf)
		return buf, bufp, err
	}
	buf = buf[:0]
	for {
		if len(buf) == cap(buf) {
			buf = append(buf, 0)[:len(buf)]
		}
		n, readErr := body.Read(buf[len(buf):cap(buf)])
		buf = buf[:len(buf)+n]
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			err = readErr
			break
		}
	}
	*bufp = buf[:0]
	return buf, bufp, err
}
