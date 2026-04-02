package config

import (
	"context"
	"crypto/sha256"
	"log/slog"
	"os"
	"sync"
	"time"
)

type Store struct {
	mu      sync.RWMutex
	path    string
	cfg     Config
	hash    [32]byte
	modTime time.Time
	log     *slog.Logger
}

func NewStore(path string, logger *slog.Logger) (*Store, error) {
	cfg, err := Load(path)
	if err != nil {
		return nil, err
	}
	b, _ := os.ReadFile(path)
	st, _ := os.Stat(path)
	return &Store{path: path, cfg: cfg, hash: sha256.Sum256(b), modTime: st.ModTime(), log: logger}, nil
}

func (s *Store) Get() Config { s.mu.RLock(); defer s.mu.RUnlock(); return s.cfg }

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
	if st.ModTime().Equal(s.modTime) {
		return
	}
	b, err := os.ReadFile(s.path)
	if err != nil {
		return
	}
	h := sha256.Sum256(b)
	if h == s.hash {
		s.modTime = st.ModTime()
		return
	}
	cfg, err := Load(s.path)
	if err != nil {
		s.log.Error("config reload failed; keeping last good", "err", err)
		return
	}
	s.cfg, s.hash, s.modTime = cfg, h, st.ModTime()
	s.log.Info("config reloaded")
}
