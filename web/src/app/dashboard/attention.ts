import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { RouterLink } from '@angular/router';

/** The single strip that surfaces problems before the cards. */
export interface Attention {
  tone: 'danger' | 'warning';
  parts: readonly string[];
  links: readonly { label: string; path: string }[];
}

/** Warning/danger strip shown above the dashboard cards. */
@Component({
  selector: 'app-dashboard-attention',
  imports: [RouterLink],
  templateUrl: './attention.html',
  styleUrl: './attention.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class DashboardAttention {
  readonly attention = input.required<Attention>();
}
