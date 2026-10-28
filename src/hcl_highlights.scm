; Terraform / HCL colours (the grammar crate ships none). Patterns don't overlap on a node:
; the inner node's colour is drawn over its parent's.

(comment) @comment

; resource "aws_s3_bucket" "logs" { … }: the block type, then its labels.
(block (identifier) @keyword)
(block (string_lit (template_literal) @type))

(attribute (identifier) @property)
(get_attr (identifier) @property)
(function_call (identifier) @function)

((variable_expr (identifier) @variable.builtin)
  (#match? @variable.builtin "^(var|local|module|data|path|terraform|count|each|self)$"))
((variable_expr (identifier) @variable)
  (#not-match? @variable "^(var|local|module|data|path|terraform|count|each|self)$"))
; Keys in maps (`tags = { Name = … }`): later patterns win on the same node.
(object_elem key: (expression (variable_expr (identifier) @property)))

(numeric_lit) @number
(bool_lit) @boolean
(null_lit) @constant.builtin

(template_interpolation_start) @punctuation.special
(template_interpolation_end) @punctuation.special
(template_directive_start) @punctuation.special
(template_directive_end) @punctuation.special
(heredoc_start) @punctuation.special
(heredoc_identifier) @punctuation.special
; Whole strings; what's inside (block labels, interpolations) is coloured on top.
(string_lit) @string
(quoted_template) @string
(heredoc_template) @string

["for" "in" "if" "else" "endif" "endfor"] @keyword

["=" "==" "!=" "<" "<=" ">" ">=" "+" "-" "*" "/" "%" "&&" "||" "!" "?" ":" "=>"] @operator
["{" "}" "[" "]" "(" ")"] @punctuation.bracket
["." "," ".*" "[*]"] @punctuation.delimiter
