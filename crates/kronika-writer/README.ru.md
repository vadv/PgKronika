# kronika-writer

[English version](README.md)

`kronika-writer` превращает ограниченные окна сбора в надёжный PGM segment. Он
владеет буферами строк, interning строк сегмента, файлом `active.parts`,
recovery и sealing. SQL-источники и byte layout находятся в других крейтах.

## Окно сбора

`SectionBuffers::push<T: Section>` хранит rows по зарегистрированному type.
При достижении `MAX_SECTION_ROWS` метод возвращает непринятую строку, чтобы
caller сбросил буфер и повторил push без потери. `flush` кодирует data sections
в порядке type id, добавляет dictionaries, вычисляет time range и возвращает
самодостаточную PGM part. После успешного flush буферы пусты.

`dict::encode` превращает текущее окно interner в отсортированные
`dict.strings` и `dict.blobs`. Snapshot rows ссылаются на них через `str_id`.

## Interner

`Interner` владеет dictionary identity открытого сегмента. Текущее окно хранит
полные bytes под `DictLimits`. После успешной записи `flush_window` заменяет их
короткой metadata для collision detection, deduplication и final placement.
Повторяющийся SQL или план не остаётся полностью продублированным до seal.

При collision, placement conflict или byte-cap failure операция не портит
предыдущее состояние. `DictError::Full` требует flush или раннего seal.

## Journal

`Journal::open(path, config)` потоково сканирует `active.parts`. Пик памяти —
одна ограниченная part, decoded catalog, bounded resync buffer и короткая
ссылка на каждую корректную part.

Оборванный финальный кадр обрезается до последней корректной границы.
Повреждение в середине или не похожее на torn tail остаётся на диске и в
`OpenReport`. `append` проверяет PGM part, пишет кадр `PGMP`, синхронизирует
файл и возвращает reference. `JournalConfig::max_journal_len` — hard cap;
следующий кадр заранее получает `JournalError::Full`.

`reset` обрезает журнал. Вызывать его можно только после успешной публикации
сегмента.

## Sealing

`seal(journal, destination)` потоково копирует section bodies в соседний
temporary file, пишет общий end catalog, синхронизирует файл и публикует hard
link. Существующий destination не перезаписывается, temporary file удаляется
при выходе.

Повторные section entries сохраняются в catalog order; writer не объединяет и
не перекодирует их. Он также не сбрасывает журнал, не выбирает имя и не
реализует retention — это lifecycle коллектора.

Ошибки журнала отделены от validation, destination и sync errors sealing.
Канонический API — [`src/lib.rs`](src/lib.rs), framing —
[`../kronika-format/`](../kronika-format/).
