package workspace

import "testing"

func TestKey(t *testing.T) {
	if got := Key("ABC/1"); got != "ABC_1" {
		t.Fatalf("got %s", got)
	}
}
