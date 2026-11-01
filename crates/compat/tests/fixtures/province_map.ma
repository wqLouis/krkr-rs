; The province action table: one action per line, `<region>: <body>`, the body
; a TJS fragment that fills the action.
;
; The region ids are the province plane's values — the palette index of the
; province image — so the table is keyed by what the plane answers.
0: autodisable = false;
16: target = "*game_start"; exp = "mapAction(16)"; onenter = "mapEnter(16)"; onleave = "mapLeave(16)";
17: target = "*game_load"; exp = "mapAction(17)"; onenter = "mapEnter(17)"; onleave = "mapLeave(17)";
