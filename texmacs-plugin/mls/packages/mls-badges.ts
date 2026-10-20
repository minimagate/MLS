<TeXmacs|2.1.5>

<style|package>

<\body>
  <assign|mls-chip|<macro|fill|line|ink|body|<with|ornament-shape|rounded|ornament-corner|100%|ornament-border|1ln|ornament-color|<arg|fill>|ornament-shadow-color|<arg|line>|ornament-sunny-color|<arg|line>|ornament-hpadding|0.45spc|ornament-vpadding|0.15spc|locus-color|preserve|<ornament|<with|color|<arg|ink>|font-family|ss|font-shape|right|font-series|medium|font-size|0.72|<arg|body>>>>>>

  <assign|mls-badges|<macro|<extern|mls-badge|<value|mls-id>|<value|color>>>>

  <assign|render-theorem|<\macro|which|body>
    <render-enunciation|<theorem-name|<arg|which><theorem-sep>><mls-badges>|<with|font-shape|italic|<arg|body>>>
  </macro>>
</body>

<\initial>
  <\collection>
    <associate|preamble|true>
  </collection>
</initial>
