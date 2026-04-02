package dashboard

import (
	"fmt"
	"strings"

	"github.com/openai/symphony/go/internal/orchestrator"
)

func RenderTerminal(s orchestrator.Snapshot) string {
	b := strings.Builder{}
	b.WriteString("RUNNING\n")
	for _, r := range s.Running {
		b.WriteString(fmt.Sprintf("- %s (%s)\n", r.Identifier, r.State))
	}
	return b.String()
}
