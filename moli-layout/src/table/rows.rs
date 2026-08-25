/// A block-size constraint collected for one CSS table row.
///
/// Row constraints are deliberately separate from both the structural table
/// boxes and Grid tracks. The initial Grid pass supplies ROWMIN; CSS Tables
/// then distributes specified section/table block sizes over these values and
/// only the resulting used lengths are projected back into the Grid backend.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct TableRowConstraint {
    pub(super) block_size: f32,
    pub(super) percent: Option<f32>,
    /// True when the row or one of its non-spanning cells has a specified
    /// block-size. Percentage rows become constrained once their percentage
    /// resolution size is definite.
    pub(super) is_constrained: bool,
}

impl TableRowConstraint {
    pub(super) fn encompass_cell(&mut self, percent: Option<f32>, is_constrained: bool) {
        self.is_constrained |= is_constrained;
        if percent > self.percent {
            self.percent = percent;
        }
    }
}

/// A contiguous row section in CSS table visual order.
///
/// This mirrors Blink's `TableTypes::Section`: headers and footers remain
/// distinct sections, while `is_body` controls which section class receives
/// remaining table height first. `specified_block_size` is kept only until
/// the initial fixed row-group minimum has been distributed into ROWMIN.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TableSectionConstraint {
    pub(super) start_row: usize,
    pub(super) row_count: usize,
    pub(super) specified_block_size: Option<f32>,
    pub(super) block_size: f32,
    pub(super) percent: Option<f32>,
    pub(super) is_constrained: bool,
    pub(super) is_body: bool,
}

/// CSS Tables caps cumulative row percentages at 100% in source order.
pub(super) fn normalize_row_percentages(rows: &mut [TableRowConstraint]) {
    let mut total = 0.0f32;
    for row in rows {
        let Some(percent) = &mut row.percent else {
            continue;
        };
        *percent = percent.max(0.0).min((1.0 - total).max(0.0));
        total += *percent;
    }
}

/// Initialize section ROWMIN values and apply fixed row-group block sizes.
/// Returns the amount by which row block sizes grew.
pub(super) fn apply_fixed_section_block_sizes(
    border_block_spacing: f32,
    sections: &mut [TableSectionConstraint],
    rows: &mut [TableRowConstraint],
) -> f32 {
    let old_total = rows.iter().map(|row| row.block_size).sum::<f32>();
    for section in sections {
        section.block_size = section_block_size(rows, *section, border_block_spacing);
        if let Some(specified_block_size) = section.specified_block_size
            && specified_block_size > section.block_size
        {
            distribute_excess_block_size_to_rows(
                section.start_row,
                section.row_count,
                specified_block_size,
                border_block_spacing,
                Some(specified_block_size),
                rows,
            );
            section.block_size = section_block_size(rows, *section, border_block_spacing);
        }
    }
    rows.iter().map(|row| row.block_size).sum::<f32>() - old_total
}

/// Distribute a specified table block-size to sections, preferring tbody-like
/// sections and then auto, fixed, and percentage constraints in that order.
pub(super) fn distribute_table_block_size_to_sections(
    border_block_spacing: f32,
    table_block_size: f32,
    sections: &mut [TableSectionConstraint],
    rows: &mut [TableRowConstraint],
) {
    if sections.is_empty() {
        return;
    }
    let spacing = border_block_spacing.max(0.0);
    let distributable_table_size =
        (table_block_size - spacing * (sections.len() + 1) as f32).max(0.0);
    let mut minimum_size_guess = sections
        .iter()
        .map(|section| section.block_size)
        .sum::<f32>();
    if distributable_table_size <= minimum_size_guess {
        return;
    }

    let mut needs_redistribution = vec![false; sections.len()];
    let percentage_sizes = sections
        .iter()
        .map(|section| {
            section
                .percent
                .map(|percent| section.block_size.max(percent * distributable_table_size))
                .unwrap_or(section.block_size)
        })
        .collect::<Vec<_>>();
    let percentage_size_guess = percentage_sizes.iter().sum::<f32>();

    // Grow every percentage section toward its target in proportion to its
    // deficit. If the percentages over-constrain the table, all deficits are
    // scaled together.
    if percentage_size_guess > minimum_size_guess {
        let amount = percentage_size_guess.min(distributable_table_size) - minimum_size_guess;
        let total_deficit = percentage_size_guess - minimum_size_guess;
        let percentage_sections = sections
            .iter()
            .enumerate()
            .filter_map(|(index, section)| section.percent.map(|_| index))
            .collect::<Vec<_>>();
        let mut remaining = amount;
        for index in percentage_sections.iter().copied() {
            let section = &mut sections[index];
            let delta = amount * (percentage_sizes[index] - section.block_size) / total_deficit;
            section.block_size += delta;
            minimum_size_guess += delta;
            remaining -= delta;
            needs_redistribution[index] = true;
        }
        if let Some(last) = percentage_sections.last().copied() {
            sections[last].block_size += remaining;
            minimum_size_guess += remaining;
        }
    }

    // Blink prefers tbody-like sections over headers and footers, then picks
    // the first non-empty class in auto/fixed/percentage order.
    let has_body = sections.iter().any(|section| section.is_body);
    let recipients_for = |body_only: bool, class: u8| {
        sections
            .iter()
            .enumerate()
            .filter_map(|(index, section)| {
                if body_only && !section.is_body {
                    return None;
                }
                let section_class = if section.percent.is_some() {
                    2
                } else if section.is_constrained {
                    1
                } else {
                    0
                };
                (section_class == class).then_some(index)
            })
            .collect::<Vec<_>>()
    };
    let mut recipients = Vec::new();
    if has_body {
        for class in 0..=2 {
            recipients = recipients_for(true, class);
            if !recipients.is_empty() {
                break;
            }
        }
    } else {
        for class in 0..=2 {
            recipients = recipients_for(false, class);
            if !recipients.is_empty() {
                break;
            }
        }
    }

    let remaining = (distributable_table_size - minimum_size_guess).max(0.0);
    if remaining > 0.0 && !recipients.is_empty() {
        let total_weight = recipients
            .iter()
            .map(|index| sections[*index].block_size)
            .sum::<f32>();
        let mut remainder = remaining;
        for index in recipients.iter().copied() {
            let delta = if total_weight > 0.0 {
                remaining * sections[index].block_size / total_weight
            } else {
                remaining / recipients.len() as f32
            };
            sections[index].block_size += delta;
            remainder -= delta;
            needs_redistribution[index] = true;
        }
        if let Some(last) = recipients.last().copied() {
            sections[last].block_size += remainder;
        }
    }

    for (index, section) in sections.iter().copied().enumerate() {
        if needs_redistribution[index] {
            distribute_excess_block_size_to_rows(
                section.start_row,
                section.row_count,
                section.block_size,
                spacing,
                Some(section.block_size),
                rows,
            );
        }
    }
}

fn section_block_size(
    rows: &[TableRowConstraint],
    section: TableSectionConstraint,
    border_block_spacing: f32,
) -> f32 {
    let end = section
        .start_row
        .saturating_add(section.row_count)
        .min(rows.len());
    rows.get(section.start_row..end)
        .unwrap_or_default()
        .iter()
        .map(|row| row.block_size)
        .sum::<f32>()
        + border_block_spacing.max(0.0) * end.saturating_sub(section.start_row + 1) as f32
}

/// Blink's table/section excess block-size distribution.
///
/// ROWMIN has already incorporated spanning cells before this stage. Blink's
/// rowspan-only second stage therefore does not participate here.
fn distribute_excess_block_size_to_rows(
    start_row: usize,
    row_count: usize,
    desired_block_size: f32,
    border_block_spacing: f32,
    percentage_resolution_block_size: Option<f32>,
    rows: &mut [TableRowConstraint],
) {
    let end_row = start_row.saturating_add(row_count).min(rows.len());
    if start_row >= end_row {
        return;
    }
    let effective_row_count = end_row - start_row;
    let mut percentage_rows_with_deficit = Vec::new();
    let mut unconstrained_non_empty_rows = Vec::new();
    let mut empty_rows = Vec::new();
    let mut unconstrained_empty_rows = Vec::new();
    let mut non_empty_rows = Vec::new();
    let mut constrained_non_empty_count = 0usize;
    let mut total_block_size = 0.0;
    let mut percentage_deficit = 0.0;
    let mut unconstrained_non_empty_size = 0.0;

    for (index, row) in rows
        .iter()
        .copied()
        .enumerate()
        .take(end_row)
        .skip(start_row)
    {
        total_block_size += row.block_size;
        let row_percentage_deficit = match (row.percent, percentage_resolution_block_size) {
            (Some(percent), Some(resolution)) if percent > 0.0 => {
                (percent * resolution - row.block_size).max(0.0)
            }
            _ => 0.0,
        };
        let is_empty = row.block_size == 0.0 && row_percentage_deficit == 0.0;
        if row_percentage_deficit > 0.0 {
            percentage_rows_with_deficit.push(index);
            percentage_deficit += row_percentage_deficit;
        }
        let is_constrained = row.is_constrained
            && (row.percent.is_none() || percentage_resolution_block_size.is_some());
        if is_empty {
            empty_rows.push(index);
            if !is_constrained {
                unconstrained_empty_rows.push(index);
            }
        } else {
            non_empty_rows.push(index);
            if is_constrained {
                constrained_non_empty_count += 1;
            } else {
                unconstrained_non_empty_rows.push(index);
                unconstrained_non_empty_size += row.block_size;
            }
        }
    }

    let internal_spacing =
        border_block_spacing.max(0.0) * effective_row_count.saturating_sub(1) as f32;
    let mut distributable =
        (desired_block_size.max(0.0) - internal_spacing - total_block_size).max(0.0);
    if distributable == 0.0 {
        return;
    }

    // 1. Percentage rows grow toward their resolved targets first.
    if percentage_deficit > 0.0 {
        let amount = distributable.min(percentage_deficit);
        let mut remaining = amount;
        for index in percentage_rows_with_deficit.iter().copied() {
            let row = &mut rows[index];
            let deficit = match (row.percent, percentage_resolution_block_size) {
                (Some(percent), Some(resolution)) => {
                    (percent * resolution - row.block_size).max(0.0)
                }
                _ => 0.0,
            };
            let delta = amount * deficit / percentage_deficit;
            row.block_size += delta;
            total_block_size += delta;
            distributable -= delta;
            remaining -= delta;
        }
        if let Some(last) = percentage_rows_with_deficit.last().copied() {
            rows[last].block_size += remaining;
            total_block_size += remaining;
            distributable -= remaining;
        }
        if distributable <= f32::EPSILON {
            return;
        }
    }

    // 3. Unconstrained non-empty rows grow in proportion to ROWMIN.
    if !unconstrained_non_empty_rows.is_empty() {
        distribute_row_growth(
            rows,
            &unconstrained_non_empty_rows,
            distributable,
            unconstrained_non_empty_size,
        );
        return;
    }

    // 4. Empty rows receive space when all rows are empty or every non-empty
    // row is constrained.
    if !empty_rows.is_empty() {
        let has_only_empty_rows = empty_rows.len() == effective_row_count;
        if has_only_empty_rows
            || empty_rows.len() + constrained_non_empty_count == effective_row_count
        {
            let recipients = if unconstrained_empty_rows.is_empty() {
                &empty_rows
            } else {
                &unconstrained_empty_rows
            };
            distribute_row_growth(rows, recipients, distributable, 0.0);
            return;
        }
    }

    // 5. With no preferred class left, all non-empty rows grow in proportion
    // to their current sizes.
    if !non_empty_rows.is_empty() {
        distribute_row_growth(rows, &non_empty_rows, distributable, total_block_size);
    }
}

fn distribute_row_growth(
    rows: &mut [TableRowConstraint],
    recipients: &[usize],
    amount: f32,
    total_weight: f32,
) {
    if recipients.is_empty() || amount <= 0.0 {
        return;
    }
    let mut remaining = amount;
    for index in recipients.iter().copied() {
        let delta = if total_weight > 0.0 {
            amount * rows[index].block_size / total_weight
        } else {
            amount / recipients.len() as f32
        };
        rows[index].block_size += delta;
        remaining -= delta;
    }
    if let Some(last) = recipients.last().copied() {
        rows[last].block_size = (rows[last].block_size + remaining).max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(block_size: f32) -> TableRowConstraint {
        TableRowConstraint {
            block_size,
            ..TableRowConstraint::default()
        }
    }

    fn section(start_row: usize, row_count: usize, block_size: f32) -> TableSectionConstraint {
        TableSectionConstraint {
            start_row,
            row_count,
            specified_block_size: None,
            block_size,
            percent: None,
            is_constrained: false,
            is_body: false,
        }
    }

    fn assert_row_sizes(rows: &[TableRowConstraint], expected: &[f32]) {
        assert_eq!(rows.len(), expected.len());
        for (index, (row, expected)) in rows.iter().zip(expected).enumerate() {
            assert!(
                (row.block_size - expected).abs() < 0.001,
                "row {index}: expected {expected}, got {}; all={rows:?}",
                row.block_size,
            );
        }
    }

    #[test]
    fn row_percentages_are_capped_in_source_order() {
        let mut rows = [
            TableRowConstraint {
                percent: Some(0.8),
                ..row(20.0)
            },
            TableRowConstraint {
                percent: Some(0.8),
                ..row(20.0)
            },
            TableRowConstraint {
                percent: Some(0.2),
                ..row(20.0)
            },
        ];

        normalize_row_percentages(&mut rows);

        assert_eq!(rows[0].percent, Some(0.8));
        assert!((rows[1].percent.unwrap_or_default() - 0.2).abs() < 0.001);
        assert_eq!(rows[2].percent, Some(0.0));
    }

    #[test]
    fn fixed_section_satisfies_percentage_rows_before_auto_rows() {
        let mut rows = [
            row(100.0),
            TableRowConstraint {
                percent: Some(0.5),
                is_constrained: true,
                ..row(100.0)
            },
            row(100.0),
        ];
        let mut sections = [TableSectionConstraint {
            specified_block_size: Some(1000.0),
            ..section(0, 3, 0.0)
        }];

        apply_fixed_section_block_sizes(0.0, &mut sections, &mut rows);

        assert_row_sizes(&rows, &[250.0, 500.0, 250.0]);
    }

    #[test]
    fn overcommitted_row_percentages_use_the_normalized_deficits() {
        let mut rows = [
            TableRowConstraint {
                percent: Some(0.8),
                is_constrained: true,
                ..row(20.0)
            },
            TableRowConstraint {
                percent: Some(0.8),
                is_constrained: true,
                ..row(20.0)
            },
            row(20.0),
        ];
        normalize_row_percentages(&mut rows);
        let mut sections = [TableSectionConstraint {
            is_body: true,
            ..section(0, 3, 80.0)
        }];

        distribute_table_block_size_to_sections(10.0, 200.0, &mut sections, &mut rows);

        assert_row_sizes(&rows, &[108.57143, 31.428572, 20.0]);
    }

    #[test]
    fn table_height_prefers_auto_body_sections() {
        let mut rows = [row(20.0), row(20.0), row(20.0)];
        let mut sections = [
            section(0, 1, 20.0),
            TableSectionConstraint {
                is_body: true,
                ..section(1, 1, 20.0)
            },
            section(2, 1, 20.0),
        ];

        distribute_table_block_size_to_sections(10.0, 300.0, &mut sections, &mut rows);

        assert_row_sizes(&rows, &[20.0, 220.0, 20.0]);
    }

    #[test]
    fn table_height_grows_percentage_sections_then_the_auto_body() {
        let mut rows = [row(20.0), row(20.0), row(20.0)];
        let mut sections = [
            TableSectionConstraint {
                percent: Some(0.5),
                is_constrained: true,
                ..section(0, 1, 20.0)
            },
            TableSectionConstraint {
                is_body: true,
                ..section(1, 1, 20.0)
            },
            TableSectionConstraint {
                percent: Some(0.25),
                is_constrained: true,
                ..section(2, 1, 20.0)
            },
        ];

        distribute_table_block_size_to_sections(10.0, 300.0, &mut sections, &mut rows);

        assert_row_sizes(&rows, &[130.0, 65.0, 65.0]);
    }
}
