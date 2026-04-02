package workflow

import (
	"context"
	"crypto/sha256"
	"log/slog"
	"os"
	"sync"
	"time"
)

type Store struct {
	mu   sync.RWMutex
	path string
	tpl  *Template
	hash [32]byte
	mod  time.Time
	log  *slog.Logger
}

func NewStore(path string, log *slog.Logger) (*Store, error) {
	t, err := Load(path)
	if err != nil {
		return nil, err
	}
	b, _ := os.ReadFile(path)
	st, _ := os.Stat(path)
	return &Store{path: path, tpl: t, hash: sha256.Sum256(b), mod: st.ModTime(), log: log}, nil
}

func (s *Store) Get() *Template { s.mu.RLock(); defer s.mu.RUnlock(); return s.tpl }

func (s *Store) Watch(ctx context.Context, every time.Duration) {
	t := time.NewTicker(every)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			s.ReloadIfChanged()
		}
	}
}

func (s *Store) ReloadIfChanged() {
	s.mu.Lock()
	defer s.mu.Unlock()
	st, err := os.Stat(s.path)
	if err != nil {
		return
	}
	if st.ModTime().Equal(s.mod) {
		return
	}
	b, err := os.ReadFile(s.path)
	if err != nil {
		return
	}
	h := sha256.Sum256(b)
	if h == s.hash {
		s.mod = st.ModTime()
		return
	}
	t, err := Load(s.path)
	if err != nil {
		s.log.Error("workflow reload failed; keeping last good", "err", err)
		return
	}
	s.tpl, s.hash, s.mod = t, h, st.ModTime()
	s.log.Info("workflow reloaded")
}
