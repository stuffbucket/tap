package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/charmbracelet/vhs/lexer"
	"github.com/charmbracelet/vhs/parser"
)

type command struct {
	Type    string `json:"type"`
	Options string `json:"options"`
	Args    string `json:"args"`
	Source  string `json:"source"`
}

type output struct {
	Commands []command `json:"commands"`
	Errors   []string  `json:"errors"`
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: tap-vhs-parse FILE.tape")
		os.Exit(2)
	}
	path, err := filepath.Abs(os.Args[1])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	// VHS resolves Source paths relative to its process working directory.
	if err := os.Chdir(filepath.Dir(path)); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	p := parser.New(lexer.New(string(data)))
	parsed := p.Parse()
	commands := make([]command, len(parsed))
	for i, cmd := range parsed {
		commands[i] = command{
			Type: cmd.Type.String(), Options: cmd.Options,
			Args: cmd.Args, Source: cmd.Source,
		}
	}
	errors := p.Errors()
	messages := make([]string, len(errors))
	for i, parseErr := range errors {
		messages[i] = parseErr.Error()
	}
	if err := json.NewEncoder(os.Stdout).Encode(output{Commands: commands, Errors: messages}); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if len(errors) > 0 {
		os.Exit(3)
	}
}
