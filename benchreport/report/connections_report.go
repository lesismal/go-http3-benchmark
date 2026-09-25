package report

import "fmt"

var (
	ConnectionsReportMarkdownHeaders = []string{}
)

// ConnectionsReport is ranked by TPS (rank:"1"), the rate the server completed
// QUIC handshakes at and answered each connection's first request, which is
// what this benchmark measures. It has no CPU EER or MEM EER to break a tie
// with: the connection test samples no CPU or memory.
type ConnectionsReport struct {
	Framework   string `json:"Framework" md:"Framework"`
	Lang        string `json:"Lang,omitempty" md:"Lang"`
	BenchClient string `json:"BenchClient" md:"Client" fmt:"client" summary:"Client"`
	Threads     int    `json:"Threads" md:"Threads" summary:"Client Threads"`
	TPS         int64  `json:"TPS" md:"TPS" rank:"1"`
	Min         int64  `json:"Min" md:"Min" fmt:"duration" tpn:"opt"`
	Avg         int64  `json:"Avg" md:"Avg" fmt:"duration" tpn:"opt"`
	Max         int64  `json:"Max" md:"Max" fmt:"duration" tpn:"opt"`
	TP50        int64  `json:"TP50" md:"-" fmt:"duration" tpn:"opt"`
	TP75        int64  `json:"TP75" md:"-" fmt:"duration" tpn:"opt"`
	TP90        int64  `json:"TP90" md:"-" fmt:"duration" tpn:"opt"`
	TP95        int64  `json:"TP95" md:"TP95" fmt:"duration" tpn:"opt"`
	TP99        int64  `json:"TP99" md:"TP99" fmt:"duration" tpn:"opt"`
	Used        int64  `json:"Used" md:"Used" fmt:"duration"`
	Total       int    `json:"Total" md:"Total"`
	Success     int64  `json:"Success" md:"Success"`
	Failed      int64  `json:"Failed" md:"Failed"`
	Concurrency int    `json:"Concurrency" md:"Concurrency" summary:"Dial Concurrency"`
}

// SetLang fills the Lang column; see ReadReports.
func (r *ConnectionsReport) SetLang(lang string) {
	r.Lang = lang
}

func (r *ConnectionsReport) Type() string {
	return "Connections"
}

func (r *ConnectionsReport) Name() string {
	return fmt.Sprintf("%s-Connections", r.Framework)
}

func (r *ConnectionsReport) Headers() []string {
	return ConnectionsReportMarkdownHeaders
}

func (r *ConnectionsReport) Fields(enableTPN bool) []string {
	return ObjFieldValues(r, enableTPN)
}

func (r *ConnectionsReport) PprofCPU() []byte {
	return nil
}

func (r *ConnectionsReport) PprofMEM() []byte {
	return nil
}

func (r *ConnectionsReport) String(enableTPN bool) string {
	return ObjString(r, enableTPN)
}
