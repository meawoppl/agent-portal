DELETE FROM turn_metrics
WHERE model IS NULL
   OR btrim(model) = ''
   OR lower(btrim(model)) = 'unknown';
