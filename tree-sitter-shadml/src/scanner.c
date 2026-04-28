// External scanner for shadml tree-sitter grammar.
//
// Emits two layout tokens:
//   LAYOUT_END       — after a newline, when the next non-blank line starts
//                      at column 0 (top-level boundary) or at EOF.
//   LAYOUT_SEMICOLON — after a newline, when the next non-blank line looks
//                      like a new binding (lowercase identifier followed
//                      eventually by '=' that isn't '==').

#include "tree_sitter/parser.h"
#include <stdbool.h>
#include <string.h>

enum TokenType {
  LAYOUT_END,
  LAYOUT_SEMICOLON,
};

void *tree_sitter_shadml_external_scanner_create(void) { return NULL; }
void tree_sitter_shadml_external_scanner_destroy(void *p) {}
unsigned tree_sitter_shadml_external_scanner_serialize(void *p, char *b) {
  return 0;
}
void tree_sitter_shadml_external_scanner_deserialize(void *p, const char *b,
                                                     unsigned n) {}

static bool is_ident_start(int32_t c) {
  return (c >= 'a' && c <= 'z') || c == '_';
}

static bool is_upper_ident_start(int32_t c) {
  return c >= 'A' && c <= 'Z';
}

static bool is_ident_continue(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
         (c >= '0' && c <= '9') || c == '_' || c == '\'';
}

static bool is_operator_char(int32_t c) {
  switch (c) {
  case '+':
  case '-':
  case '*':
  case '/':
  case '%':
  case '=':
  case '<':
  case '>':
  case '!':
  case '&':
  case '^':
  case '|':
  case '~':
    return true;
  default:
    return false;
  }
}

static bool is_decl_keyword(const char *ident, unsigned len) {
  return (len == 4 && strncmp(ident, "data", 4) == 0) ||
         (len == 5 && strncmp(ident, "alias", 5) == 0) ||
         (len == 6 && strncmp(ident, "extern", 6) == 0) ||
         (len == 5 && strncmp(ident, "trait", 5) == 0) ||
         (len == 4 && strncmp(ident, "impl", 4) == 0) ||
         (len == 8 && strncmp(ident, "bitfield", 8) == 0) ||
         (len == 5 && strncmp(ident, "const", 5) == 0) ||
         (len == 4 && strncmp(ident, "when", 4) == 0) ||
         (len == 6 && strncmp(ident, "import", 6) == 0) ||
         (len == 6 && strncmp(ident, "module", 6) == 0) ||
         (len == 4 && strncmp(ident, "else", 4) == 0) ||
         (len == 9 && strncmp(ident, "immediate", 9) == 0) ||
         (len == 6 && strncmp(ident, "render", 6) == 0) ||
         (len == 7 && strncmp(ident, "storage", 7) == 0);
}

bool tree_sitter_shadml_external_scanner_scan(void *payload, TSLexer *lexer,
                                              const bool *valid_symbols) {
  bool want_end = valid_symbols[LAYOUT_END];
  bool want_semi = valid_symbols[LAYOUT_SEMICOLON];
  if (!want_end && !want_semi) return false;

  // Skip whitespace, tracking newlines.
  bool saw_newline = false;

  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\n') {
    if (lexer->lookahead == '\n') {
      saw_newline = true;
    }
    lexer->advance(lexer, true);  // skip whitespace
  }

  if (!saw_newline) return false;

  uint32_t next_col = lexer->get_column(lexer);
  bool at_eof = lexer->eof(lexer);

  // LAYOUT_END: column 0 or EOF.
  if (want_end && (at_eof || next_col == 0)) {
    lexer->result_symbol = LAYOUT_END;
    return true;
  }

  // LAYOUT_SEMICOLON: lookahead to check if this line starts a new declaration
  // or binding inside an indented layout block.
  if (want_semi && !at_eof && next_col > 0) {
    // Mark the end of the token HERE (zero-width, after whitespace).
    // All further advances are just lookahead — they won't be part of the token.
    lexer->mark_end(lexer);

    if (lexer->lookahead == '@') {
      // Only emit LAYOUT_SEMICOLON for @group and @binding, which start
      // binding declarations/entries. Other @foo (attributes like @vertex)
      // should not trigger a semicolon so they can attach to the next
      // declaration.
      lexer->advance(lexer, false);
      char word[16];
      unsigned word_len = 0;
      while (is_ident_continue(lexer->lookahead) && word_len < sizeof(word) - 1) {
        word[word_len++] = (char)lexer->lookahead;
        lexer->advance(lexer, false);
      }
      word[word_len] = '\0';
      if ((word_len == 5 && strncmp(word, "group", 5) == 0) ||
          (word_len == 7 && strncmp(word, "binding", 7) == 0)) {
        lexer->result_symbol = LAYOUT_SEMICOLON;
        return true;
      }
      return false;
    }

    if (lexer->lookahead == '(') {
      lexer->advance(lexer, false);
      bool saw_operator = false;
      while (is_operator_char(lexer->lookahead)) {
        saw_operator = true;
        lexer->advance(lexer, false);
      }
      if (!saw_operator || lexer->lookahead != ')') {
        return false;
      }
      lexer->advance(lexer, false);
    } else if (is_ident_start(lexer->lookahead) ||
               is_upper_ident_start(lexer->lookahead)) {
      char ident[32];
      unsigned ident_len = 0;
      while (is_ident_continue(lexer->lookahead)) {
        if (ident_len + 1 < sizeof(ident)) {
          ident[ident_len++] = (char)lexer->lookahead;
        }
        lexer->advance(lexer, false);
      }
      ident[ident_len] = '\0';

      if (is_decl_keyword(ident, ident_len)) {
        lexer->result_symbol = LAYOUT_SEMICOLON;
        return true;
      }
    } else {
      return false;
    }

    // Scan forward on the same line for ':' or a bare '=' at depth 0.
    int depth = 0;
    for (int i = 0; i < 200; i++) {
      int32_t c = lexer->lookahead;
      if (c == '\n' || c == '\r' || c == 0) break;

      if (c == ':' && depth == 0) {
        lexer->result_symbol = LAYOUT_SEMICOLON;
        return true;
      }

      if (c == '=' && depth == 0) {
        lexer->advance(lexer, false);
        if (lexer->lookahead != '=') {
          lexer->result_symbol = LAYOUT_SEMICOLON;
          return true;
        }
        break;
      }

      if (c == '(') depth++;
      if (c == ')') {
        if (depth > 0) depth--;
        else break;
      }

      if (c == '-') {
        lexer->advance(lexer, false);
        if (lexer->lookahead == '>') continue;
        continue;
      }

      lexer->advance(lexer, false);
    }
  }

  return false;
}
