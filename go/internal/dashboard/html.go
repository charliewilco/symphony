package dashboard

import (
	"fmt"
	"strings"

	"github.com/openai/symphony/go/internal/orchestrator"
)

func RenderHTML(s orchestrator.Snapshot) string {
	rows := []string{"<table><tr><th>Issue</th><th>State</th></tr>"}
	for _, r := range s.Running {
		rows = append(rows, fmt.Sprintf("<tr><td>%s</td><td>%s</td></tr>", r.Identifier, r.State))
	}
	rows = append(rows, "</table>")
	return "<html><body>" + strings.Join(rows, "") + "</body></html>"
}
