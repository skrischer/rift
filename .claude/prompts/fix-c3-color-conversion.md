Du arbeitest auf dem Branch `worktree-feat+phase-1.5-gpui` im Repo `/home/developer/CascadeProjects/rift`.

In `crates/terminal/src/view.rs` (Zeilen 774-782) gibt es `RgbaExt::to_hsla` — ein manuelles Bit-Packing von `gpui::Rgba` Float-Kanaelen in ein u32, das dann an `gpui::rgba(u32)` uebergeben und per `.into()` zu `Hsla` konvertiert wird. Das ist ein sinnloser Roundtrip (`Rgba` -> u32 -> `Rgba` -> `Hsla`) und hat einen Overflow-Bug (`(1.0 * 255.0) as u32` kann 256 produzieren).

`gpui::Rgba` implementiert `Into<Hsla>` direkt. Der gesamte `RgbaExt` Helper ist unnoetig.

Fix:

1. Loesche die `RgbaExt` struct und ihre `to_hsla` Methode komplett
2. Ersetze alle Aufrufe `RgbaExt::to_hsla(rgba)` durch `rgba.into()` (oder `Hsla::from(rgba)`)
3. Pruefe ob `colors::to_gpui_color()` den Rueckgabetyp aendern sollte — wenn alle Callsites sowieso `Hsla` brauchen, koennte `to_gpui_color` direkt `Hsla` zurueckgeben und den `.into()` Call an einer Stelle zentralisieren

Aufrufe von `RgbaExt::to_hsla` befinden sich in view.rs an ca. 5 Stellen (Zeilen 263, 568, 569, 657, 691). `CellRenderInfo` in grid.rs speichert `fg` und `bg` als `Rgba` — pruefe ob diese zu `Hsla` geaendert werden sollten, damit die Konversion nur einmal bei der Extraktion stattfindet statt bei jedem Render.

Lies die Dateien, verstehe die Color-Pipeline (alacritty Color -> colors.rs -> grid.rs CellRenderInfo -> view.rs paint), und vereinfache sie. Nicht committen.
