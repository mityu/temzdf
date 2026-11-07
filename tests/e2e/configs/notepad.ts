// Text panes edit without modes (the default editor is Vim's): for
// scenarios of that editing (edit.actions).
import notepad from "temzdf/edit/notepad";
import vim from "temzdf/edit/vim";
vim.disable();
notepad.enable();
