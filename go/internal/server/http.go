package server

import (
	"encoding/json"
	"net/http"
	"strings"

	"github.com/openai/symphony/go/internal/dashboard"
	"github.com/openai/symphony/go/internal/orchestrator"
)

type API struct {
	Orch    *orchestrator.Service
	Refresh func()
}

func (a API) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/api/state", func(w http.ResponseWriter, r *http.Request) { _ = json.NewEncoder(w).Encode(a.Orch.Snapshot()) })
	mux.HandleFunc("/api/state/", func(w http.ResponseWriter, r *http.Request) {
		id := strings.TrimPrefix(r.URL.Path, "/api/state/")
		s := a.Orch.Snapshot()
		for _, item := range s.Running {
			if item.IssueID == id {
				_ = json.NewEncoder(w).Encode(item)
				return
			}
		}
		http.NotFound(w, r)
	})
	mux.HandleFunc("/api/refresh", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			w.WriteHeader(405)
			return
		}
		a.Refresh()
		w.WriteHeader(202)
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(dashboard.RenderHTML(a.Orch.Snapshot())))
	})
	return mux
}
