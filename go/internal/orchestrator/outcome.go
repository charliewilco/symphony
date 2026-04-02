package orchestrator

type AgentOutcome int

const (
	OutcomeDone AgentOutcome = iota
	OutcomeContinue
	OutcomeBlocked
	OutcomeFailed
)

type AgentResult struct {
	Outcome AgentOutcome
	Error   error
	Reason  string
}
